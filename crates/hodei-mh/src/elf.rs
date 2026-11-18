//! Minimal ELF inspection: does this binary need a DYNAMIC LOADER?
//!
//! A distributable binary must be STANDALONE: with no `PT_INTERP`
//! segment the kernel loads the image directly, so it needs no glibc,
//! no libgcc and no other shared library from the host. That is what
//! makes the release usable on a distro whose glibc is older than the
//! one the binary was built against.
//!
//! The check is a fact read from the file, never an assumption about
//! how it was built.

/// ELF64 program header types.
const PT_INTERP: u32 = 3;

/// ELF class: 64-bit.
const ELFCLASS64: u8 = 2;

/// ELF data encoding: little endian, the only one the headers are read
/// with. A big endian image is not parsed as if it were little endian:
/// it is refused.
const ELFDATA2LSB: u8 = 1;

/// The machine types the release names in its artifacts.
const EM_X86_64: u16 = 62;
const EM_AARCH64: u16 = 183;

/// How the binary is linked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Linkage {
    /// No dynamic loader is involved: the binary stands alone.
    Standalone,
    /// A dynamic loader is required: the binary needs system libraries.
    NeedsDynamicLoader,
}

impl Linkage {
    /// Whether the binary can be distributed without the host's
    /// libraries.
    pub fn is_standalone(&self) -> bool {
        matches!(self, Linkage::Standalone)
    }
}

/// Whether the ELF image needs a dynamic loader.
///
/// Returns `NeedsDynamicLoader` for anything that is not a readable
/// ELF64 image: an unknown format is never reported as standalone.
pub fn linkage(bytes: &[u8]) -> Linkage {
    // ELF64 header: e_ident[16], e_type[2], e_machine[2], e_version[4],
    // e_entry[8], e_phoff[8] at offset 0x20, e_shoff[8], e_flags[4],
    // e_ehsize[2], e_phentsize[2] at 0x36, e_phnum[2] at 0x38.
    const ELF_MAGIC: &[u8; 4] = b"\x7fELF";
    const PH_OFFSET: usize = 0x20;
    const PHENTSIZE_OFFSET: usize = 0x36;
    const PHNUM_OFFSET: usize = 0x38;
    const PHENT_SIZE: usize = 56;

    if bytes.len() < 0x3A
        || &bytes[0..4] != ELF_MAGIC
        || bytes[4] != ELFCLASS64
        || bytes[5] != ELFDATA2LSB
    {
        return Linkage::NeedsDynamicLoader;
    }
    let u16_at = |offset: usize| -> Option<u64> {
        let end = offset.checked_add(2)?;
        let slice = bytes.get(offset..end)?;
        Some(u16::from_le_bytes([slice[0], slice[1]]) as u64)
    };
    let u64_at = |offset: usize| -> Option<u64> {
        let end = offset.checked_add(8)?;
        let slice = bytes.get(offset..end)?;
        Some(u64::from_le_bytes(slice.try_into().ok()?))
    };
    let phoff = match u64_at(PH_OFFSET) {
        Some(value) => value as usize,
        None => return Linkage::NeedsDynamicLoader,
    };
    let phentsize = match u16_at(PHENTSIZE_OFFSET) {
        Some(value) => value as usize,
        None => return Linkage::NeedsDynamicLoader,
    };
    let phnum = match u16_at(PHNUM_OFFSET) {
        Some(value) => value as usize,
        None => return Linkage::NeedsDynamicLoader,
    };
    if phentsize < PHENT_SIZE {
        return Linkage::NeedsDynamicLoader;
    }
    for index in 0..phnum {
        let Some(offset) = index.checked_mul(phentsize) else {
            return Linkage::NeedsDynamicLoader;
        };
        let Some(base) = phoff.checked_add(offset) else {
            return Linkage::NeedsDynamicLoader;
        };
        // The header type is four bytes: a truncated or absurd offset
        // is refused, never read past the end of the image.
        let Some(end) = base.checked_add(4) else {
            return Linkage::NeedsDynamicLoader;
        };
        let Some(header) = bytes.get(base..end) else {
            return Linkage::NeedsDynamicLoader;
        };
        let p_type = u32::from_le_bytes([header[0], header[1], header[2], header[3]]);
        if p_type == PT_INTERP {
            return Linkage::NeedsDynamicLoader;
        }
    }
    Linkage::Standalone
}

/// The architecture the image was built for, read from its header.
///
/// `None` for anything that is not a readable little endian ELF64
/// image: an unknown architecture is never named.
pub fn architecture(bytes: &[u8]) -> Option<&'static str> {
    const ELF_MAGIC: &[u8; 4] = b"\x7fELF";
    const MACHINE_OFFSET: usize = 0x12;
    if bytes.len() < 0x3A || &bytes[0..4] != ELF_MAGIC || bytes[4] != ELFCLASS64 {
        return None;
    }
    if bytes[5] != ELFDATA2LSB {
        return None;
    }
    let slice = bytes.get(MACHINE_OFFSET..MACHINE_OFFSET.checked_add(2)?)?;
    let machine = u16::from_le_bytes([slice[0], slice[1]]);
    match machine {
        EM_X86_64 => Some("x86_64"),
        EM_AARCH64 => Some("aarch64"),
        _ => None,
    }
}

/// Read a file and report the architecture it was built for.
pub fn architecture_of(path: &std::path::Path) -> Option<&'static str> {
    std::fs::read(path)
        .ok()
        .and_then(|bytes| architecture(&bytes))
}

/// Read a file and report how it is linked. A file that cannot be read
/// is NOT standalone: an unreadable artifact is never a distribution.
pub fn linkage_of(path: &std::path::Path) -> Linkage {
    match std::fs::read(path) {
        Ok(bytes) => linkage(&bytes),
        Err(_) => Linkage::NeedsDynamicLoader,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal ELF64 header with `phnum` program headers.
    fn elf(phnum: u16, interp: bool) -> Vec<u8> {
        const PHOFF: usize = 0x40;
        let mut bytes = vec![0u8; PHOFF];
        bytes[0..4].copy_from_slice(b"\x7fELF");
        bytes[4] = ELFCLASS64;
        bytes[5] = 1; // little endian
        bytes[0x20..0x28].copy_from_slice(&(PHOFF as u64).to_le_bytes()); // e_phoff
        bytes[0x36..0x38].copy_from_slice(&(56u16).to_le_bytes()); // e_phentsize
        bytes[0x38..0x3A].copy_from_slice(&phnum.to_le_bytes()); // e_phnum
        let mut out = bytes;
        out.resize(PHOFF + 56 * phnum as usize, 0);
        if interp {
            out[PHOFF..PHOFF + 4].copy_from_slice(&PT_INTERP.to_le_bytes());
        }
        out
    }

    #[test]
    fn an_image_without_an_interpreter_is_standalone() {
        assert_eq!(linkage(&elf(2, false)), Linkage::Standalone);
        assert!(linkage(&elf(2, false)).is_standalone());
    }

    #[test]
    fn an_image_with_an_interpreter_needs_the_dynamic_loader() {
        assert_eq!(linkage(&elf(2, true)), Linkage::NeedsDynamicLoader);
        assert!(!linkage(&elf(2, true)).is_standalone());
    }

    #[test]
    fn something_that_is_not_an_elf_is_never_standalone() {
        assert_eq!(linkage(b""), Linkage::NeedsDynamicLoader);
        assert_eq!(
            linkage(b"#!/bin/sh\necho hello"),
            Linkage::NeedsDynamicLoader
        );
        assert_eq!(linkage(&[0u8; 128]), Linkage::NeedsDynamicLoader);
    }

    #[test]
    fn an_image_in_another_data_encoding_is_not_read_as_little_endian() {
        // The headers are read little endian: a big endian image would
        // be read as garbage, so it is refused instead.
        let mut bytes = elf(2, false);
        bytes[5] = 2; // big endian
        assert_eq!(linkage(&bytes), Linkage::NeedsDynamicLoader);
        assert_eq!(architecture(&bytes), None);
    }

    #[test]
    fn the_architecture_is_read_from_the_header() {
        let mut bytes = elf(2, false);
        bytes[0x12..0x14].copy_from_slice(&EM_X86_64.to_le_bytes());
        assert_eq!(architecture(&bytes), Some("x86_64"));
        bytes[0x12..0x14].copy_from_slice(&EM_AARCH64.to_le_bytes());
        assert_eq!(architecture(&bytes), Some("aarch64"));
        bytes[0x12..0x14].copy_from_slice(&7u16.to_le_bytes());
        assert_eq!(architecture(&bytes), None);
    }

    #[test]
    fn an_absurd_program_header_offset_is_refused_and_never_panics() {
        // The offset comes from the file: it can be anything. The
        // inspection refuses instead of reading past the image (and
        // without overflowing on the way).
        let mut bytes = elf(2, false);
        bytes[0x20..0x28].copy_from_slice(&u64::MAX.to_le_bytes());
        assert_eq!(linkage(&bytes), Linkage::NeedsDynamicLoader);
        bytes[0x20..0x28].copy_from_slice(&(usize::MAX as u64).to_le_bytes());
        assert_eq!(linkage(&bytes), Linkage::NeedsDynamicLoader);
    }
}
