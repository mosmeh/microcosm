use crate::{
    boot::{Bootable, EBDA_START, HIGH_MEMORY_START},
    memory::{CopyToGuest, RangeAllocator},
    Error, KernelParams, Result,
};
use std::{
    ffi::{CStr, CString},
    mem::size_of,
};
use sys::{
    bootparam::{boot_e820_entry, boot_params, setup_header, CAN_USE_HEAP},
    e820::E820_RAM,
    elf,
    elfnote::XEN_ELFNOTE_PHYS32_ENTRY,
    kvm_bindings::{kvm_regs, kvm_sregs},
    multiboot::{
        multiboot_info_t, multiboot_memory_map_t, multiboot_module_t, MULTIBOOT_BOOTLOADER_MAGIC,
        MULTIBOOT_HEADER_MAGIC, MULTIBOOT_INFO_ALIGN, MULTIBOOT_INFO_CMDLINE,
        MULTIBOOT_INFO_MEM_MAP, MULTIBOOT_INFO_MODS, MULTIBOOT_MEMORY_AVAILABLE,
        MULTIBOOT_MOD_ALIGN, MULTIBOOT_SEARCH,
    },
    start_info::{
        hvm_memmap_table_entry, hvm_start_info, XEN_HVM_MEMMAP_TYPE_RAM, XEN_HVM_START_MAGIC_VALUE,
    },
};
use zerocopy::FromBytes;

// Specifications for the boot protocols:
// - Linux: https://www.kernel.org/doc/Documentation/x86/boot.txt
// - PVH: https://xenbits.xen.org/docs/unstable/misc/pvh.html
// - Multiboot: https://www.gnu.org/software/grub/manual/multiboot/multiboot.html

#[derive(Debug, Clone, Copy)]
pub enum BootProtocol {
    /// 32-bit Linux
    Linux32,

    /// 64-bit Linux
    Linux64,

    /// PVH
    Pvh,

    /// Multiboot
    Multiboot,
}

impl BootProtocol {
    pub fn is_32bit(self) -> bool {
        matches!(self, Self::Linux32 | Self::Pvh | Self::Multiboot)
    }

    pub fn configure_sregs(self, sregs: &mut kvm_sregs) {
        if matches!(self, Self::Pvh) {
            // cr0: bit 0 (PE) must be set.
            //      All the other writeable bits are cleared.
            sregs.cr0 = 1;

            // cr4: all bits are cleared.
            sregs.cr4 = 0;
        }
    }

    pub fn configure_regs(self, regs: &mut kvm_regs, params_addr: u64) {
        match self {
            Self::Linux32 => {
                // %esi must hold the base address of the struct boot_params
                regs.rsi = params_addr;

                // %ebp, %edi and %ebx must be zero
                regs.rbp = 0;
                regs.rdi = 0;
                regs.rbx = 0;
            }
            Self::Linux64 => {
                // %rsi must hold the base address of the struct boot_params
                regs.rsi = params_addr;
            }
            Self::Pvh => {
                // ebx: contains the physical memory address where the loader
                //      has placed the boot start info structure.
                regs.rbx = params_addr;

                // eflags: bit 17 (VM) must be cleared.
                //         Bit 9 (IF) must be cleared.
                //         Bit 8 (TF) must be cleared.
                //         Other bits are all unspecified.
                regs.rflags &= !(1 << 8 | 1 << 9 | 1 << 17);
            }
            Self::Multiboot => {
                // 'EAX' Must contain the magic value ‘0x2BADB002’
                regs.rax = MULTIBOOT_BOOTLOADER_MAGIC.into();

                // 'EBX' Must contain the 32-bit physical address of
                //       the Multiboot information structure provided by
                //       the boot loader
                regs.rbx = params_addr;

                // 'EFLAGS' Bit 17 (VM) must be cleared. Bit 9 (IF) must be
                //          cleared. Other bits are all undefined.
                regs.rflags &= !(1 << 9 | 1 << 17);
            }
        }
    }
}

impl Bootable {
    pub fn load(memory: &mut [u8], kernel: &[u8], params: KernelParams) -> Result<Self> {
        if let Ok(exe) = load_elf64(memory, kernel) {
            if let Ok(bootable) = load_pvh(memory, kernel, exe.max_addr, params.clone()) {
                return Ok(bootable);
            }

            // Assume it's vmlinux.
            let params_addr =
                write_linux_boot_params(memory, default_setup_header(), exe.max_addr, params)?;
            return Ok(Self {
                protocol: BootProtocol::Linux64,
                entry_addr: exe.entry_addr,
                params_addr,
            });
        }

        if let Ok(exe) = load_elf32(memory, kernel) {
            let count = kernel.len().min(MULTIBOOT_SEARCH as usize) / size_of::<u32>();
            let (slice, _) = u32::slice_from_prefix(kernel, count).unwrap();
            if slice.iter().any(|&magic| magic == MULTIBOOT_HEADER_MAGIC) {
                let params_addr = write_multiboot_info(memory, exe.max_addr, params)?;
                return Ok(Self {
                    protocol: BootProtocol::Multiboot,
                    entry_addr: exe.entry_addr,
                    params_addr,
                });
            }

            // Assume it's vmlinux.
            let params_addr =
                write_linux_boot_params(memory, default_setup_header(), exe.max_addr, params)?;
            return Ok(Self {
                protocol: BootProtocol::Linux32,
                entry_addr: exe.entry_addr,
                params_addr,
            });
        }

        if let Ok(bootable) = load_bz_image(memory, kernel, params) {
            return Ok(bootable);
        }

        Err(Error::InvalidKernelImageFormat)
    }
}

struct LoadedExecutable {
    entry_addr: u64,
    max_addr: u64,
}

fn load_elf32(memory: &mut [u8], image: &[u8]) -> Result<LoadedExecutable> {
    let ehdr = elf::Elf32_Ehdr::read_from_prefix(image).ok_or(Error::InvalidKernelImageFormat)?;
    if ehdr.e_ident[elf::EI_MAG0 as usize] != elf::ELFMAG0 as u8
        || ehdr.e_ident[elf::EI_MAG1 as usize] != elf::ELFMAG1
        || ehdr.e_ident[elf::EI_MAG2 as usize] != elf::ELFMAG2
        || ehdr.e_ident[elf::EI_MAG3 as usize] != elf::ELFMAG3
        || ehdr.e_ident[elf::EI_CLASS as usize] != elf::ELFCLASS32 as u8
        || ehdr.e_ident[elf::EI_DATA as usize] != elf::ELFDATA2LSB as u8
        || ehdr.e_phentsize as usize != size_of::<elf::Elf32_Phdr>()
        || (ehdr.e_phoff as usize) < size_of::<elf::Elf32_Ehdr>()
    {
        return Err(Error::InvalidKernelImageFormat);
    }

    let (phdrs, _) =
        elf::Elf32_Phdr::slice_from_prefix(&image[ehdr.e_phoff as usize..], ehdr.e_phnum as usize)
            .ok_or(Error::InvalidKernelImageFormat)?;
    let mut max_addr = 0;
    for phdr in phdrs {
        if phdr.p_type != elf::PT_LOAD {
            continue;
        }
        image[phdr.p_offset as usize..][..phdr.p_filesz as usize]
            .copy_to_guest(memory, phdr.p_paddr)?;
        memory[phdr.p_paddr as usize..][phdr.p_filesz as usize..phdr.p_memsz as usize].fill(0);
        max_addr = max_addr.max(phdr.p_paddr + phdr.p_memsz);
    }

    Ok(LoadedExecutable {
        entry_addr: ehdr.e_entry.into(),
        max_addr: max_addr.into(),
    })
}

fn load_elf64(memory: &mut [u8], image: &[u8]) -> Result<LoadedExecutable> {
    let ehdr = elf::Elf64_Ehdr::read_from_prefix(image).ok_or(Error::InvalidKernelImageFormat)?;
    if ehdr.e_ident[elf::EI_MAG0 as usize] != elf::ELFMAG0 as u8
        || ehdr.e_ident[elf::EI_MAG1 as usize] != elf::ELFMAG1
        || ehdr.e_ident[elf::EI_MAG2 as usize] != elf::ELFMAG2
        || ehdr.e_ident[elf::EI_MAG3 as usize] != elf::ELFMAG3
        || ehdr.e_ident[elf::EI_CLASS as usize] != elf::ELFCLASS64 as u8
        || ehdr.e_ident[elf::EI_DATA as usize] != elf::ELFDATA2LSB as u8
        || ehdr.e_phentsize as usize != size_of::<elf::Elf64_Phdr>()
        || (ehdr.e_phoff as usize) < size_of::<elf::Elf64_Ehdr>()
    {
        return Err(Error::InvalidKernelImageFormat);
    }

    let (phdrs, _) =
        elf::Elf64_Phdr::slice_from_prefix(&image[ehdr.e_phoff as usize..], ehdr.e_phnum as usize)
            .ok_or(Error::InvalidKernelImageFormat)?;
    let mut max_addr = 0;
    for phdr in phdrs {
        if phdr.p_type != elf::PT_LOAD {
            continue;
        }
        image[phdr.p_offset as usize..][..phdr.p_filesz as usize]
            .copy_to_guest(memory, phdr.p_paddr)?;
        memory[phdr.p_paddr as usize..][phdr.p_filesz as usize..phdr.p_memsz as usize].fill(0);
        max_addr = max_addr.max(phdr.p_paddr + phdr.p_memsz);
    }

    Ok(LoadedExecutable {
        entry_addr: ehdr.e_entry,
        max_addr,
    })
}

const SETUP_HEADER_MAGIC: u32 = 0x5372_6448; // "HdrS"

fn load_bz_image(memory: &mut [u8], kernel: &[u8], params: KernelParams) -> Result<Bootable> {
    let boot_params =
        boot_params::read_from_prefix(kernel).ok_or(Error::InvalidKernelImageFormat)?;
    let setup_header {
        mut setup_sects,
        header,
        version,
        loadflags,
        code32_start,
        ..
    } = boot_params.hdr;
    if header != SETUP_HEADER_MAGIC || version < 0x206 || loadflags & 1 == 0 {
        return Err(Error::InvalidKernelImageFormat);
    }

    if setup_sects == 0 {
        setup_sects = 4;
    }
    let setup_size = (setup_sects as usize + 1) << 9;
    let image = &kernel[setup_size..];
    image.copy_to_guest(memory, HIGH_MEMORY_START)?;

    let max_addr = HIGH_MEMORY_START + image.len() as u64;
    let params_addr = write_linux_boot_params(memory, boot_params.hdr, max_addr, params)?;

    // Both 32-bit and 64-bit bzImage can be booted with the same protocol.
    Ok(Bootable {
        protocol: BootProtocol::Linux32,
        entry_addr: code32_start.into(),
        params_addr,
    })
}

fn load_pvh(
    memory: &mut [u8],
    image: &[u8],
    exe_end: u64,
    params: KernelParams,
) -> Result<Bootable> {
    let ehdr = elf::Elf64_Ehdr::read_from_prefix(image).ok_or(Error::InvalidKernelImageFormat)?;
    let (phdrs, _) =
        elf::Elf64_Phdr::slice_from_prefix(&image[ehdr.e_phoff as usize..], ehdr.e_phnum as usize)
            .ok_or(Error::InvalidKernelImageFormat)?;
    let mut entry = None;
    'outer: for phdr in phdrs {
        if phdr.p_type != elf::PT_NOTE {
            continue;
        }
        let mut offset = phdr.p_offset as usize;
        while offset < (phdr.p_offset + phdr.p_filesz) as usize {
            let nhdr = elf::Elf64_Nhdr::ref_from_prefix(&image[offset..]).unwrap();
            offset += size_of::<elf::Elf64_Nhdr>();

            let name = &image[offset..][..nhdr.n_namesz as usize];
            offset += nhdr.n_namesz.next_multiple_of(4) as usize;

            let desc = &image[offset..][..nhdr.n_descsz as usize];
            offset += nhdr.n_descsz.next_multiple_of(4) as usize;

            if name == b"Xen\0" && nhdr.n_type == XEN_ELFNOTE_PHYS32_ENTRY {
                entry = Some(u32::read_from_prefix(desc).ok_or(Error::InvalidKernelImageFormat)?);
                break 'outer;
            }
        }
    }
    let entry_addr = entry.ok_or(Error::InvalidKernelImageFormat)?;

    let mut allocator = RangeAllocator::new(exe_end);
    let params_addr = allocator.alloc::<hvm_start_info>();
    let cmdline_paddr = if let Some(cmdline) = params.cmdline {
        let cmdline = cmdline.to_bytes_with_nul();
        let addr = allocator.alloc_array::<u8>(cmdline.len());
        cmdline.copy_to_guest(memory, addr)?;
        addr
    } else {
        0
    };
    let memmap_entries = [
        hvm_memmap_table_entry {
            addr: 0,
            size: EBDA_START,
            type_: XEN_HVM_MEMMAP_TYPE_RAM,
            reserved: 0,
        },
        hvm_memmap_table_entry {
            addr: HIGH_MEMORY_START,
            size: memory.len() as u64 - HIGH_MEMORY_START,
            type_: XEN_HVM_MEMMAP_TYPE_RAM,
            reserved: 0,
        },
    ];
    let memmap_paddr = allocator.alloc_array::<hvm_memmap_table_entry>(memmap_entries.len());
    memmap_entries.copy_to_guest(memory, memmap_paddr)?;

    let start_info = hvm_start_info {
        magic: XEN_HVM_START_MAGIC_VALUE,
        version: 1,
        cmdline_paddr,
        memmap_paddr,
        memmap_entries: memmap_entries.len() as u32,
        ..Default::default()
    };
    start_info.copy_to_guest(memory, params_addr)?;

    Ok(Bootable {
        protocol: BootProtocol::Pvh,
        entry_addr: entry_addr.into(),
        params_addr,
    })
}

fn write_linux_boot_params(
    memory: &mut [u8],
    mut hdr: setup_header,
    exe_end: u64,
    params: KernelParams,
) -> Result<u64> {
    hdr.type_of_loader = 0xff;
    hdr.loadflags |= CAN_USE_HEAP as u8;
    hdr.heap_end_ptr = 0xfe00;

    let mut allocator = RangeAllocator::new(exe_end);

    if let Some(cmdline) = params.cmdline {
        let max_len = hdr.cmdline_size as usize;
        if cmdline.as_bytes().len() > max_len {
            return Err(Error::CmdlineTooLong {
                len: cmdline.as_bytes().len(),
                max_len,
            });
        }
        let addr = allocator.alloc_array::<u8>(cmdline.as_bytes_with_nul().len());
        cmdline.as_bytes_with_nul().copy_to_guest(memory, addr)?;
        hdr.cmd_line_ptr = addr as u32;
    }

    if let Some(initrd_path) = params.initrd_path {
        let bytes = std::fs::read(initrd_path)?;
        let addr = allocator.raw_alloc(bytes.len(), 0x0010_0000);
        if addr > hdr.initrd_addr_max.into() {
            return Err(Error::InitrdTooLarge {
                size: bytes.len(),
                max_size: hdr.initrd_addr_max as usize - addr as usize,
            });
        }
        bytes.copy_to_guest(memory, addr)?;
        hdr.ramdisk_image = addr as u32;
        hdr.ramdisk_size = bytes.len() as u32;
    }

    let mut boot_params = boot_params {
        hdr,
        ..Default::default()
    };

    let mut add_e820_entry = |addr: u64, size: u64, type_: u32| {
        boot_params.e820_table[boot_params.e820_entries as usize] =
            boot_e820_entry { addr, size, type_ };
        boot_params.e820_entries += 1;
    };
    add_e820_entry(0, EBDA_START, E820_RAM);
    add_e820_entry(
        HIGH_MEMORY_START,
        memory.len() as u64 - HIGH_MEMORY_START,
        E820_RAM,
    );

    let zero_page_addr = allocator.alloc::<boot_params>();
    boot_params.copy_to_guest(memory, zero_page_addr)?;

    Ok(zero_page_addr)
}

fn default_setup_header() -> setup_header {
    setup_header {
        boot_flag: 0xaa55,
        header: SETUP_HEADER_MAGIC,
        type_of_loader: 0xff,
        initrd_addr_max: 0x37ff_ffff,
        kernel_alignment: 0x0100_0000,
        cmdline_size: 255,
        ..Default::default()
    }
}

fn write_multiboot_info(memory: &mut [u8], exe_end: u64, params: KernelParams) -> Result<u64> {
    let mut allocator = RangeAllocator::new(exe_end);

    let info_addr =
        allocator.raw_alloc(size_of::<multiboot_info_t>(), MULTIBOOT_INFO_ALIGN as usize);
    let mods_addr = allocator.alloc_array::<multiboot_module_t>(params.module_paths.len());
    let mmap_addr = allocator.alloc::<multiboot_memory_map_t>();
    let mut info = multiboot_info_t {
        flags: MULTIBOOT_INFO_MODS | MULTIBOOT_INFO_MEM_MAP,
        mods_count: params.module_paths.len() as u32,
        mods_addr: mods_addr as u32,
        mmap_addr: mmap_addr as u32,
        mmap_length: size_of::<multiboot_memory_map_t>() as u32,
        ..Default::default()
    };

    if let Some(cmdline) = params.cmdline {
        let cmdline = cmdline.as_bytes_with_nul();
        let addr = allocator.alloc_array::<u8>(cmdline.len());
        info.cmdline = addr as u32;
        info.flags |= MULTIBOOT_INFO_CMDLINE;
        cmdline.copy_to_guest(memory, addr)?;
    }

    info.copy_to_guest(memory, info_addr)?;

    let mut mod_entry_addr = mods_addr;
    for module_path in params.module_paths {
        let module_bytes = std::fs::read(&module_path)?;
        let module_path = module_path.to_string_lossy();
        let module_path = module_path.as_bytes();
        let module_path = CStr::from_bytes_until_nul(module_path)
            .map_or_else(|_| CString::new(module_path).unwrap(), ToOwned::to_owned);
        let module_path = module_path.as_bytes_with_nul();

        let mod_start = allocator.raw_alloc(module_bytes.len(), MULTIBOOT_MOD_ALIGN as usize);
        let mod_end = mod_start + module_bytes.len() as u64;
        let cmdline = allocator.alloc_array::<u8>(module_path.len());
        multiboot_module_t {
            mod_start: mod_start as u32,
            mod_end: mod_end as u32,
            cmdline: cmdline as u32,
            pad: 0,
        }
        .copy_to_guest(memory, mod_entry_addr)?;
        module_bytes.copy_to_guest(memory, mod_start)?;
        module_path.copy_to_guest(memory, cmdline)?;

        mod_entry_addr += size_of::<multiboot_module_t>() as u64;
    }

    multiboot_memory_map_t {
        size: size_of::<multiboot_memory_map_t>() as u32,
        addr: HIGH_MEMORY_START,
        len: memory.len() as u64 - HIGH_MEMORY_START,
        type_: MULTIBOOT_MEMORY_AVAILABLE,
    }
    .copy_to_guest(memory, mmap_addr)?;

    Ok(info_addr)
}
