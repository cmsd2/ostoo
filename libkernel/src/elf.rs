//! Minimal ELF64 parser for static (ET_EXEC) x86-64 binaries.

use alloc::vec::Vec;

// ---------------------------------------------------------------------------
// ELF64 header and program header (C layout)

#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct Elf64Ehdr {
    e_ident:     [u8; 16],
    e_type:      u16,
    e_machine:   u16,
    e_version:   u32,
    e_entry:     u64,
    e_phoff:     u64,
    e_shoff:     u64,
    e_flags:     u32,
    e_ehsize:    u16,
    e_phentsize: u16,
    e_phnum:     u16,
    e_shentsize: u16,
    e_shnum:     u16,
    e_shstrndx:  u16,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct Elf64Phdr {
    p_type:   u32,
    p_flags:  u32,
    p_offset: u64,
    p_vaddr:  u64,
    p_paddr:  u64,
    p_filesz: u64,
    p_memsz:  u64,
    p_align:  u64,
}

// ELF constants
const ELF_MAGIC: [u8; 4] = [0x7F, b'E', b'L', b'F'];
const ELFCLASS64: u8 = 2;
const ELFDATA2LSB: u8 = 1;
const ET_EXEC: u16 = 2;
const EM_X86_64: u16 = 62;
const PT_LOAD: u32 = 1;

/// Segment permission flags from the ELF program header.
pub const PF_X: u32 = 1;
pub const PF_W: u32 = 2;
#[allow(dead_code)]
pub const PF_R: u32 = 4;

// ---------------------------------------------------------------------------
// Public types

#[derive(Debug)]
pub enum ElfError {
    TooSmall,
    BadMagic,
    Not64Bit,
    NotLittleEndian,
    NotExec,
    NotX86_64,
    BadPhdr,
}

#[derive(Debug)]
pub struct LoadSegment {
    /// Virtual address where this segment should be loaded.
    pub vaddr: u64,
    /// Offset within the ELF file where segment data begins.
    pub offset: u64,
    /// Number of bytes to copy from the file.
    pub filesz: u64,
    /// Total size in memory (filesz..memsz is zero-filled).
    pub memsz: u64,
    /// ELF permission flags (PF_R, PF_W, PF_X).
    pub flags: u32,
}

#[derive(Debug)]
pub struct ElfInfo {
    pub entry: u64,
    pub segments: Vec<LoadSegment>,
}

// ---------------------------------------------------------------------------
// Parser

pub fn parse(data: &[u8]) -> Result<ElfInfo, ElfError> {
    let ehdr_size = core::mem::size_of::<Elf64Ehdr>();
    if data.len() < ehdr_size {
        return Err(ElfError::TooSmall);
    }

    // Safety: Elf64Ehdr is repr(C) and we've checked the length.
    let ehdr: Elf64Ehdr = unsafe { core::ptr::read_unaligned(data.as_ptr() as *const Elf64Ehdr) };

    if ehdr.e_ident[0..4] != ELF_MAGIC {
        return Err(ElfError::BadMagic);
    }
    if ehdr.e_ident[4] != ELFCLASS64 {
        return Err(ElfError::Not64Bit);
    }
    if ehdr.e_ident[5] != ELFDATA2LSB {
        return Err(ElfError::NotLittleEndian);
    }
    if ehdr.e_type != ET_EXEC {
        return Err(ElfError::NotExec);
    }
    if ehdr.e_machine != EM_X86_64 {
        return Err(ElfError::NotX86_64);
    }

    let phdr_size = core::mem::size_of::<Elf64Phdr>();
    let mut segments = Vec::new();

    for i in 0..ehdr.e_phnum as usize {
        let off = ehdr.e_phoff as usize + i * ehdr.e_phentsize as usize;
        if off + phdr_size > data.len() {
            return Err(ElfError::BadPhdr);
        }
        let phdr: Elf64Phdr =
            unsafe { core::ptr::read_unaligned(data.as_ptr().add(off) as *const Elf64Phdr) };

        if phdr.p_type != PT_LOAD {
            continue;
        }

        // Validate that file data is within bounds.
        let end = phdr.p_offset.checked_add(phdr.p_filesz).ok_or(ElfError::BadPhdr)?;
        if end as usize > data.len() {
            return Err(ElfError::BadPhdr);
        }

        segments.push(LoadSegment {
            vaddr: phdr.p_vaddr,
            offset: phdr.p_offset,
            filesz: phdr.p_filesz,
            memsz: phdr.p_memsz,
            flags: phdr.p_flags,
        });
    }

    Ok(ElfInfo {
        entry: ehdr.e_entry,
        segments,
    })
}
