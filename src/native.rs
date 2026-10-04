//! Native symbol cache, jump tables, and stubs adapted from Dioxus CLI
//! build/patch.rs at 57d6794, under its Apache-2.0 option. See NOTICE.
//! Modified to support ELF and Mach-O and use the GPUI driver's error handling.

use crate::platform::Platform;
use anyhow::{Context, Result, ensure};
use object::{
    Object, ObjectSection, ObjectSymbol, SymbolFlags, SymbolKind, SymbolScope,
    read::File,
    write::{SectionId, StandardSection, Symbol, SymbolSection},
};
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
};
use subsecond_types::{AddressMap, JumpTable};

pub struct Cache {
    symbols: HashMap<String, CachedSymbol>,
    tls: Vec<u8>,
    tls_inits: HashMap<String, Vec<u8>>,
    pub platform: Platform,
    build_version: Option<object::write::MachOBuildVersion>,
    architecture: object::Architecture,
}

struct CachedSymbol {
    address: u64,
    kind: SymbolKind,
    undefined: bool,
    weak: bool,
    size: u64,
    flags: SymbolFlags<SectionId, object::write::SymbolId>,
}

impl Cache {
    pub fn new(path: &Path) -> Result<Self> {
        let bytes = std::fs::read(path)?;
        let file = File::parse(&*bytes)?;
        let platform = Platform::from_format(file.format())?;
        ensure!(
            matches!(
                file.architecture(),
                object::Architecture::X86_64 | object::Architecture::Aarch64
            ),
            "unsupported native architecture"
        );
        let symbols = file
            .symbols()
            .filter_map(|symbol| {
                Some((
                    symbol.name().ok()?.into(),
                    CachedSymbol {
                        address: symbol.address(),
                        kind: symbol.kind(),
                        undefined: symbol.is_undefined(),
                        weak: symbol.is_weak(),
                        size: symbol.size(),
                        flags: match symbol.flags() {
                            SymbolFlags::Elf { st_info, st_other } => {
                                SymbolFlags::Elf { st_info, st_other }
                            }
                            SymbolFlags::MachO { n_desc } => SymbolFlags::MachO { n_desc },
                            _ => SymbolFlags::None,
                        },
                    },
                ))
            })
            .collect::<HashMap<_, _>>();
        ensure!(
            symbols.contains_key(platform.main_symbol()),
            "missing main symbol; debug symbols are required"
        );
        let tls = file
            .sections()
            .find(|section| section.name() == Ok(".tdata"))
            .and_then(|section| section.data().ok())
            .unwrap_or_default()
            .to_vec();
        // Mach-O has no symbol sizes. Infer TLS initializer extents from adjacent
        // symbols, including zero-initialized TLS in __thread_bss.
        let mut tls_inits = HashMap::new();
        if platform == Platform::Macos {
            for section in file
                .sections()
                .filter(|section| matches!(section.name(), Ok("__thread_data" | "__thread_bss")))
            {
                let mut initializers: Vec<_> = file
                    .symbols()
                    .filter(|symbol| symbol.section_index() == Some(section.index()))
                    .filter_map(|symbol| Some((symbol.address(), symbol.name().ok()?)))
                    .collect();
                initializers.sort_unstable();
                let data = section.data()?;
                for (i, &(address, name)) in initializers.iter().enumerate() {
                    if !name.ends_with("$tlv$init") {
                        continue;
                    }
                    let next = initializers[i + 1..]
                        .iter()
                        .find(|(next, _)| *next > address)
                        .map(|(next, _)| *next)
                        .unwrap_or(section.address() + section.size());
                    let start = usize::try_from(
                        address
                            .checked_sub(section.address())
                            .context("TLS initializer is below its section")?,
                    )?;
                    let size = usize::try_from(
                        next.checked_sub(address)
                            .context("invalid TLS initializer extent")?,
                    )?;
                    ensure!(size > 0, "empty Mach-O TLS initializer {name}");
                    let init = if section.kind() == object::SectionKind::UninitializedTls {
                        vec![0; size]
                    } else {
                        data.get(start..start.checked_add(size).context("TLS size overflow")?)
                            .context("TLS initializer outside section")?
                            .to_vec()
                    };
                    tls_inits.insert(name.to_owned(), init);
                }
            }
        }
        let build_version = if let File::MachO64(macho) = &file {
            let mut version = object::write::MachOBuildVersion::default();
            version.platform = object::macho::PLATFORM_MACOS;
            version.minos = 11 << 16;
            version.sdk = 11 << 16;
            if let Some(build) = macho.build_version()? {
                version.platform = build.platform.get(macho.endian());
                version.minos = build.minos.get(macho.endian());
                version.sdk = build.sdk.get(macho.endian());
            }
            ensure!(
                version.platform == object::macho::PLATFORM_MACOS,
                "only macOS Mach-O binaries are supported"
            );
            Some(version)
        } else {
            None
        };
        Ok(Self {
            symbols,
            tls,
            tls_inits,
            platform,
            build_version,
            architecture: file.architecture(),
        })
    }

    pub fn jump_table(&self, patch: &Path) -> Result<JumpTable> {
        let bytes = std::fs::read(patch)?;
        let file = File::parse(&*bytes)?;
        ensure!(
            file.format() == self.platform.format() && file.architecture() == self.architecture,
            "patch format or architecture differs from the baseline"
        );
        let symbols = file.symbol_map();
        let mut map = AddressMap::default();
        for symbol in symbols.symbols() {
            if let Some(old) = self.symbols.get(symbol.name())
                && !old.undefined
            {
                map.insert(old.address, symbol.address());
            }
        }
        let main = symbols
            .symbols()
            .iter()
            .find(|s| s.name() == self.platform.main_symbol())
            .context("missing main symbol in patch")?;
        Ok(JumpTable {
            lib: patch.to_owned(),
            map,
            aslr_reference: self.symbols[self.platform.main_symbol()].address,
            new_base_address: main.address(),
            ifunc_count: 0,
        })
    }

    pub fn stub(&self, objects: &[PathBuf], runtime_main: u64) -> Result<Vec<u8>> {
        let mut undefined = HashSet::new();
        let mut defined = HashSet::new();
        for path in objects {
            let bytes = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
            let file = File::parse(&*bytes)?;
            for symbol in file.symbols() {
                if symbol.is_undefined() {
                    undefined.insert(symbol.name()?.to_owned());
                } else if symbol.is_global() {
                    defined.insert(symbol.name()?.to_owned());
                }
            }
        }
        let slide = runtime_main
            .checked_sub(self.symbols[self.platform.main_symbol()].address)
            .context("runtime main address is below the baseline main symbol")?;
        let mut obj = object::write::Object::new(
            self.platform.format(),
            self.architecture,
            object::Endianness::Little,
        );
        if let Some(version) = self.build_version {
            obj.set_macho_build_version(version);
        }
        let text = obj.section_id(StandardSection::Text);
        // Sort symbols to make the generated object deterministic.
        let mut names: Vec<_> = undefined.difference(&defined).collect();
        names.sort();
        for name in names {
            let Some(symbol) = self.symbols.get(name) else {
                continue;
            };
            if symbol.undefined {
                continue;
            }
            let address = symbol
                .address
                .checked_add(slide)
                .context("symbol address overflow")?;
            // object::write adds the Mach-O global prefix itself.
            let stub_name = if self.platform == Platform::Macos {
                name.strip_prefix('_')
                    .context("missing Mach-O symbol prefix")?
            } else {
                name.as_str()
            };
            match symbol.kind {
                SymbolKind::Text => {
                    let mut code = match self.architecture {
                        object::Architecture::X86_64 => vec![0xff, 0x25, 0, 0, 0, 0], // jmp [rip+0]
                        object::Architecture::Aarch64 => vec![0x50, 0, 0, 0x58, 0, 2, 0x1f, 0xd6], // ldr x16; br x16
                        _ => unreachable!(),
                    };
                    code.extend_from_slice(&address.to_le_bytes());
                    let offset = obj.append_section_data(text, &code, 8);
                    obj.add_symbol(Symbol {
                        name: stub_name.as_bytes().to_vec(),
                        value: offset,
                        size: code.len() as u64,
                        scope: SymbolScope::Linkage,
                        kind: SymbolKind::Text,
                        weak: false,
                        section: SymbolSection::Section(text),
                        flags: SymbolFlags::None,
                    });
                }
                SymbolKind::Tls => {
                    // Patches have fresh TLS, initialized from the baseline.
                    let init = if self.platform == Platform::Macos {
                        self.tls_inits
                            .get(&format!("{name}$tlv$init"))
                            .with_context(|| {
                                format!(
                                    "missing Mach-O TLS initializer for {name}; rebuild the app"
                                )
                            })?
                            .clone()
                    } else {
                        let size = if symbol.size > 0 { symbol.size } else { 8 };
                        let start = usize::try_from(symbol.address)?;
                        let end = start
                            .checked_add(usize::try_from(size)?)
                            .context("TLS size overflow")?;
                        if end <= self.tls.len() {
                            self.tls[start..end].to_vec()
                        } else {
                            vec![0; usize::try_from(size)?]
                        }
                    };
                    let size = init.len() as u64;
                    let section = obj.section_id(StandardSection::Tls);
                    let id = obj.add_symbol(Symbol {
                        name: stub_name.as_bytes().to_vec(),
                        value: 0,
                        size: 0,
                        scope: SymbolScope::Linkage,
                        kind: SymbolKind::Tls,
                        weak: false,
                        section: SymbolSection::Undefined,
                        flags: SymbolFlags::None,
                    });
                    obj.add_symbol_data(id, section, &init, size.min(8).next_power_of_two());
                }
                kind => {
                    obj.add_symbol(Symbol {
                        name: stub_name.as_bytes().to_vec(),
                        value: address,
                        size: 0,
                        scope: SymbolScope::Linkage,
                        kind: if kind == SymbolKind::Unknown {
                            SymbolKind::Data
                        } else {
                            kind
                        },
                        weak: symbol.weak,
                        section: SymbolSection::Absolute,
                        flags: symbol.flags,
                    });
                }
            }
        }
        Ok(obj.write()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use object::write::Object as WritableObject;
    use std::sync::atomic::{AtomicUsize, Ordering};
    static NEXT: AtomicUsize = AtomicUsize::new(0);

    fn object(main: u64, foo: u64) -> Vec<u8> {
        let mut obj = WritableObject::new(
            object::BinaryFormat::Elf,
            object::Architecture::X86_64,
            object::Endianness::Little,
        );
        let text = obj.section_id(StandardSection::Text);
        obj.append_section_data(text, &[0; 128], 8);
        for (name, address) in [("main", main), ("foo", foo)] {
            obj.add_symbol(Symbol {
                name: name.as_bytes().to_vec(),
                value: address,
                size: 1,
                kind: SymbolKind::Text,
                scope: SymbolScope::Linkage,
                weak: false,
                section: SymbolSection::Section(text),
                flags: SymbolFlags::None,
            });
        }
        obj.add_symbol(Symbol {
            name: b"global".to_vec(),
            value: 0x2000,
            size: 8,
            kind: SymbolKind::Data,
            scope: SymbolScope::Linkage,
            weak: false,
            section: SymbolSection::Absolute,
            flags: SymbolFlags::None,
        });
        obj.write().unwrap()
    }

    #[test]
    fn stubs_use_runtime_slide_and_leave_external_symbols_unresolved() {
        let dir = std::env::temp_dir().join(format!(
            "gpui-stub-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let baseline = dir.join("base.o");
        std::fs::write(&baseline, object(16, 32)).unwrap();
        let cache = Cache::new(&baseline).unwrap();
        let mut input = WritableObject::new(
            object::BinaryFormat::Elf,
            object::Architecture::X86_64,
            object::Endianness::Little,
        );
        for name in ["foo", "global", "external"] {
            input.add_symbol(Symbol {
                name: name.as_bytes().to_vec(),
                value: 0,
                size: 0,
                kind: SymbolKind::Unknown,
                scope: SymbolScope::Linkage,
                weak: false,
                section: SymbolSection::Undefined,
                flags: SymbolFlags::None,
            });
        }
        let path = dir.join("input.o");
        std::fs::write(&path, input.write().unwrap()).unwrap();
        let bytes = cache.stub(&[path], 0x10010).unwrap();
        let stub = File::parse(&*bytes).unwrap();
        let foo = stub.symbols().find(|s| s.name() == Ok("foo")).unwrap();
        let section = stub.section_by_index(foo.section_index().unwrap()).unwrap();
        let code = &section.data().unwrap()[foo.address() as usize..][..14];
        assert_eq!(&code[..6], &[0xff, 0x25, 0, 0, 0, 0]);
        assert_eq!(u64::from_le_bytes(code[6..].try_into().unwrap()), 0x10020);
        assert_eq!(
            stub.symbols()
                .find(|s| s.name() == Ok("global"))
                .unwrap()
                .address(),
            0x12000
        );
        assert!(!stub.symbols().any(|s| s.name() == Ok("external")));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn jump_table_maps_baseline_addresses_to_patch_offsets() {
        let dir = std::env::temp_dir().join(format!(
            "gpui-table-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let base = dir.join("base.o");
        let patch = dir.join("patch.o");
        std::fs::write(&base, object(16, 32)).unwrap();
        std::fs::write(&patch, object(48, 64)).unwrap();
        let table = Cache::new(&base).unwrap().jump_table(&patch).unwrap();
        assert_eq!(table.aslr_reference, 16);
        assert_eq!(table.new_base_address, 48);
        assert_eq!(table.map[&32], 64);
        assert_eq!(table.lib, patch);
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn macho_stubs_preserve_names_tls_descriptors_and_build_version() {
        for architecture in [object::Architecture::X86_64, object::Architecture::Aarch64] {
            let dir = std::env::temp_dir().join(format!(
                "gpui-macho-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&dir).unwrap();
            let mut base = WritableObject::new(
                object::BinaryFormat::MachO,
                architecture,
                object::Endianness::Little,
            );
            let mut version = object::write::MachOBuildVersion::default();
            version.platform = object::macho::PLATFORM_MACOS;
            version.minos = 13 << 16;
            version.sdk = 15 << 16;
            base.set_macho_build_version(version);
            let text = base.section_id(StandardSection::Text);
            base.append_section_data(text, &[0; 128], 8);
            for (name, value) in [("main", 16), ("foo", 32)] {
                base.add_symbol(Symbol {
                    name: name.as_bytes().to_vec(),
                    value,
                    size: 1,
                    kind: SymbolKind::Text,
                    scope: SymbolScope::Linkage,
                    weak: false,
                    section: SymbolSection::Section(text),
                    flags: SymbolFlags::None,
                });
            }
            base.add_symbol(Symbol {
                name: b"global".to_vec(),
                value: 0x2000,
                size: 8,
                kind: SymbolKind::Data,
                scope: SymbolScope::Linkage,
                weak: false,
                section: SymbolSection::Absolute,
                flags: SymbolFlags::None,
            });
            let tls = base.section_id(StandardSection::Tls);
            let tbss = base.section_id(StandardSection::UninitializedTls);
            for (name, init, section) in [
                ("TLS", vec![0x42; 24], tls),
                ("NEXT_TLS", vec![0x19; 16], tls),
                ("ZERO_TLS", vec![0; 40], tbss),
            ] {
                let id = base.add_symbol(Symbol {
                    name: name.as_bytes().to_vec(),
                    value: 0,
                    size: 0,
                    kind: SymbolKind::Tls,
                    scope: SymbolScope::Linkage,
                    weak: false,
                    section: SymbolSection::Undefined,
                    flags: SymbolFlags::None,
                });
                if section == tbss {
                    base.add_symbol_bss(id, section, init.len() as u64, 8);
                } else {
                    base.add_symbol_data(id, section, &init, 8);
                }
            }
            let baseline = dir.join("base.o");
            std::fs::write(&baseline, base.write().unwrap()).unwrap();
            let cache = Cache::new(&baseline).unwrap();
            assert_eq!(cache.tls_inits["_TLS$tlv$init"], vec![0x42; 24]);
            assert_eq!(cache.tls_inits["_ZERO_TLS$tlv$init"], vec![0; 40]);
            let mut input = WritableObject::new(
                object::BinaryFormat::MachO,
                architecture,
                object::Endianness::Little,
            );
            for name in ["foo", "global", "TLS", "ZERO_TLS", "external"] {
                input.add_symbol(Symbol {
                    name: name.as_bytes().to_vec(),
                    value: 0,
                    size: 0,
                    kind: SymbolKind::Data,
                    scope: SymbolScope::Linkage,
                    weak: false,
                    section: SymbolSection::Undefined,
                    flags: SymbolFlags::None,
                });
            }
            let input_path = dir.join("input.o");
            std::fs::write(&input_path, input.write().unwrap()).unwrap();
            let bytes = cache.stub(&[input_path], 0x10010).unwrap();
            let stub = File::parse(&*bytes).unwrap();
            let foo = stub.symbols().find(|s| s.name() == Ok("_foo")).unwrap();
            let code = stub
                .section_by_index(foo.section_index().unwrap())
                .unwrap()
                .data()
                .unwrap();
            let offset = foo.address() as usize;
            let width = if architecture == object::Architecture::X86_64 {
                6
            } else {
                8
            };
            assert_eq!(
                u64::from_le_bytes(code[offset + width..offset + width + 8].try_into().unwrap()),
                0x10020
            );
            assert_eq!(
                stub.symbols()
                    .find(|s| s.name() == Ok("_global"))
                    .unwrap()
                    .address(),
                0x12000
            );
            assert!(!stub.symbols().any(|s| s.name() == Ok("_external")));
            assert!(
                stub.symbols()
                    .any(|s| s.name() == Ok("__tlv_bootstrap") && s.is_undefined())
            );
            for name in ["_TLS", "_ZERO_TLS"] {
                let symbol = stub.symbols().find(|s| s.name() == Ok(name)).unwrap();
                let section = stub
                    .section_by_index(symbol.section_index().unwrap())
                    .unwrap();
                assert_eq!(section.name().unwrap(), "__thread_vars");
                assert_eq!(section.relocations().count(), 4);
            }
            assert_eq!(
                stub.section_by_name("__thread_data")
                    .unwrap()
                    .data()
                    .unwrap(),
                [vec![0x42; 24], vec![0; 40]].concat()
            );
            let File::MachO64(macho) = stub else {
                panic!("not Mach-O")
            };
            let build = macho.build_version().unwrap().unwrap();
            assert_eq!(build.minos.get(macho.endian()), 13 << 16);
            assert_eq!(build.sdk.get(macho.endian()), 15 << 16);
            let table = cache.jump_table(&baseline).unwrap();
            assert_eq!(table.aslr_reference, 16);
            assert_eq!(table.new_base_address, 16);
            assert_eq!(table.map[&32], 32);
            std::fs::remove_dir_all(dir).unwrap();
        }
    }
}
