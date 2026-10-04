use anyhow::{Result, bail};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Platform {
    Linux,
    Macos,
}

impl Platform {
    pub fn host() -> Result<Self> {
        match std::env::consts::OS {
            "linux" => Ok(Self::Linux),
            "macos" => Ok(Self::Macos),
            _ => bail!("native watch currently supports Linux and macOS only"),
        }
    }

    pub fn from_format(format: object::BinaryFormat) -> Result<Self> {
        match format {
            object::BinaryFormat::Elf => Ok(Self::Linux),
            object::BinaryFormat::MachO => Ok(Self::Macos),
            _ => bail!("native watch requires an ELF or Mach-O binary"),
        }
    }

    pub fn format(self) -> object::BinaryFormat {
        match self {
            Self::Linux => object::BinaryFormat::Elf,
            Self::Macos => object::BinaryFormat::MachO,
        }
    }

    pub fn main_symbol(self) -> &'static str {
        match self {
            Self::Linux => "main",
            Self::Macos => "_main",
        }
    }

    pub fn library_extension(self) -> &'static str {
        match self {
            Self::Linux => "so",
            Self::Macos => "dylib",
        }
    }
}
