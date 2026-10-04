//! Native symbol cache, jump tables, and stubs adapted from Dioxus CLI
//! build/patch.rs at 57d6794, under its Apache-2.0 option. See NOTICE.
//! Modified to support Linux ELF only and use the GPUI driver's error handling.

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
        ensure!(
            file.format() == object::BinaryFormat::Elf,
            "only Linux ELF binaries are supported"
        );
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
                            _ => SymbolFlags::None,
                        },
                    },
                ))
            })
            .collect::<HashMap<_, _>>();
        ensure!(
            symbols.contains_key("main"),
            "missing main symbol; debug symbols are required"
        );
        let tls = file
            .sections()
            .find(|section| section.name() == Ok(".tdata"))
            .and_then(|section| section.data().ok())
            .unwrap_or_default()
            .to_vec();
        Ok(Self {
            symbols,
            tls,
            architecture: file.architecture(),
        })
    }

    pub fn jump_table(&self, patch: &Path) -> Result<JumpTable> {
        let bytes = std::fs::read(patch)?;
        let file = File::parse(&*bytes)?;
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
            .find(|s| s.name() == "main")
            .context("missing main symbol in patch")?;
        Ok(JumpTable {
            lib: patch.to_owned(),
            map,
            aslr_reference: self.symbols["main"].address,
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
            .checked_sub(self.symbols["main"].address)
            .context("runtime main address is below the ELF main symbol")?;
        let mut obj = object::write::Object::new(
            object::BinaryFormat::Elf,
            self.architecture,
            object::Endianness::Little,
        );
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
                        name: name.as_bytes().to_vec(),
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
                    // ELF TLS addresses are offsets in .tdata. Like upstream, patches
                    // get a fresh TLS copy, initialized from the baseline binary.
                    let size = if symbol.size > 0 { symbol.size } else { 8 };
                    let start = usize::try_from(symbol.address)?;
                    let end = start
                        .checked_add(usize::try_from(size)?)
                        .context("TLS size overflow")?;
                    let init = if end <= self.tls.len() {
                        self.tls[start..end].to_vec()
                    } else {
                        vec![0; usize::try_from(size)?]
                    };
                    let section = obj.section_id(StandardSection::Tls);
                    let id = obj.add_symbol(Symbol {
                        name: name.as_bytes().to_vec(),
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
                        name: name.as_bytes().to_vec(),
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
}
