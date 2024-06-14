use crate::{
    load::BootProtocol,
    memory::{CopyToGuest, RangeAllocator},
    Result,
};
use std::{ffi::c_char, mem::size_of};
use sys::{
    acpi::{
        acpi_madt_io_apic, acpi_madt_local_apic, acpi_madt_type_ACPI_MADT_TYPE_IO_APIC,
        acpi_madt_type_ACPI_MADT_TYPE_LOCAL_APIC, acpi_subtable_header, acpi_table_header,
        acpi_table_madt, acpi_table_rsdp, ACPI_MADT_ENABLED, ACPI_RSDP_CHECKSUM_LENGTH,
        ACPI_SIG_MADT, ACPI_SIG_RSDP, ACPI_SIG_XSDT,
    },
    kvm_bindings::{kvm_regs, kvm_segment, kvm_sregs},
};
use zerocopy::AsBytes;

#[derive(Clone)]
pub struct Bootable {
    pub protocol: BootProtocol,
    pub entry_addr: u64,
    pub params_addr: u64,
}

impl Bootable {
    pub fn configure_memory(&self, memory: &mut [u8]) -> Result<()> {
        if self.protocol.is_32bit() {
            GDT32.copy_to_guest(memory, GDT_BASE)?;
            IDT32.copy_to_guest(memory, IDT_BASE)?;

            // The structure of the page table is as follows:
            //    # | Kind  |   Size | Memory range
            // -----|-------|--------|-------------
            //    n | PDE   | (4*n)B |  memory_size
            // 1024 | PTE   |   4KiB |         4MiB

            let n = memory.len().div_ceil(0x0040_0000) as u32;
            let pde_addr = PAGE_TABLE_ADDR as u32;
            let pte_addr = pde_addr + 0x1000;
            for pde in 0u32..n {
                ((pte_addr + (pde << 12)) | 0x3) // P | RW
                    .copy_to_guest(
                        memory,
                        u64::from(pde_addr) + u64::from(pde) * size_of::<u32>() as u64,
                    )?;
            }
            for pte in 0u32..1024 * n {
                ((pte << 12) | 0x3) // P | RW
                    .copy_to_guest(
                        memory,
                        u64::from(pte_addr) + u64::from(pte) * size_of::<u32>() as u64,
                    )?;
            }
        } else {
            GDT64.copy_to_guest(memory, GDT_BASE)?;
            IDT64.copy_to_guest(memory, IDT_BASE)?;

            // The structure of the page table is as follows:
            //   # | Kind  | Size | Memory range
            // ----|-------|------|-------------
            //   1 | PML4E |   8B |         4GiB
            //   4 | PDPTE |  32B |         1GiB
            // 512 | PDE   | 4KiB |         2MiB

            let pml4_addr = PAGE_TABLE_ADDR;
            let pdpte_addr = pml4_addr + 0x1000;
            let pde_addr = pdpte_addr + 0x1000;
            (pdpte_addr | 0x3) // P | RW
                .copy_to_guest(memory, pml4_addr)?;
            for pdpte in 0u64..4 {
                ((pde_addr + (pdpte << 12)) | 0x3) // P | RW
                    .copy_to_guest(
                         memory,pdpte_addr  + pdpte  * size_of::<u64>() as u64
                    )?;
            }
            for pde in 0u64..4 * 512 {
                ((pde << 21) | 0x83) // P | RW | PS
                    .copy_to_guest(
                         memory,pde_addr  + pde  * size_of::<u64>() as u64
                    )?;
            }
        }
        Ok(())
    }

    pub fn configure_sregs(&self, sregs: &mut kvm_sregs) {
        sregs.ds = DATA_SEGMENT.kvm_segment();
        sregs.es = DATA_SEGMENT.kvm_segment();
        sregs.fs = DATA_SEGMENT.kvm_segment();
        sregs.gs = DATA_SEGMENT.kvm_segment();
        sregs.ss = DATA_SEGMENT.kvm_segment();
        sregs.tr = TSS_SEGMENT.kvm_segment();
        sregs.gdt.base = GDT_BASE;
        sregs.idt.base = IDT_BASE;
        sregs.cr3 = PAGE_TABLE_ADDR;

        if self.protocol.is_32bit() {
            sregs.cs = CODE_SEGMENT32.kvm_segment();
            sregs.gdt.limit = GDT32.as_bytes().len() as u16 - 1;
            sregs.idt.limit = IDT32.as_bytes().len() as u16 - 1;
            sregs.cr0 |= 0x1; // PE
            sregs.cr0 &= !0x8000_0000; // PG
            sregs.cr4 &= !0x20; // PAE
            sregs.efer &= !0x500; // LME | LMA
        } else {
            sregs.cs = CODE_SEGMENT64.kvm_segment();
            sregs.gdt.limit = GDT64.as_bytes().len() as u16 - 1;
            sregs.idt.limit = IDT64.as_bytes().len() as u16 - 1;
            sregs.cr0 |= 0x8000_0001; // PE | PG
            sregs.cr4 |= 0x20; // PAE
            sregs.efer |= 0x500; // LME | LMA
        }
        self.protocol.configure_sregs(sregs);
    }

    pub fn configure_regs(&self, regs: &mut kvm_regs) {
        regs.rflags = 0x2;
        regs.rip = self.entry_addr;
        regs.rsp = STACK_POINTER;
        self.protocol.configure_regs(regs, self.params_addr);
    }
}

pub fn configure_acpi(memory: &mut [u8], num_cpus: usize) -> Result<()> {
    macro_rules! signature {
        ($($c:expr)*) => {[$($c as c_char,)*]};
        ($s:expr; 4) => {signature!($s[0] $s[1] $s[2] $s[3])};
        ($s:expr; 8) => {signature!($s[0] $s[1] $s[2] $s[3] $s[4] $s[5] $s[6] $s[7])};
    }

    macro_rules! checksum {
        ($($x:expr),*) => {{
            let mut sum = 0u8;
            $(
                for &b in $x.as_bytes() {
                    sum = sum.wrapping_add(b);
                }
            )*
            (u8::MAX - sum).wrapping_add(1)
        }};
    }

    let mut allocator = RangeAllocator::new(RSDP_ADDR);
    let xsdp_size = size_of::<acpi_table_rsdp>();
    let xsdp_addr = allocator.raw_alloc(xsdp_size, 16);
    assert_eq!(xsdp_addr, RSDP_ADDR);

    let xsdt_size = size_of::<acpi_table_header>() + size_of::<u64>();
    let xsdt_addr = allocator.raw_alloc(xsdt_size, 1);

    let madt_size = size_of::<acpi_table_madt>()
        + size_of::<acpi_madt_io_apic>()
        + num_cpus * size_of::<acpi_madt_local_apic>();
    let madt_addr = allocator.raw_alloc(madt_size, 1);

    let mut xsdp = acpi_table_rsdp {
        signature: signature!(ACPI_SIG_RSDP; 8),
        revision: 2, // ACPI 2.0 or later
        length: xsdp_size as u32,
        xsdt_physical_address: xsdt_addr,
        ..Default::default()
    };
    xsdp.checksum = checksum!(xsdp.as_bytes()[..ACPI_RSDP_CHECKSUM_LENGTH as usize]);
    xsdp.extended_checksum = checksum!(xsdp);
    xsdp.copy_to_guest(memory, xsdp_addr)?;

    let mut xsdt_header = acpi_table_header {
        signature: signature!(ACPI_SIG_XSDT; 4),
        length: xsdt_size as u32,
        revision: 1,
        ..Default::default()
    };
    xsdt_header.checksum = checksum!(xsdt_header, madt_addr);
    xsdt_header.copy_to_guest(memory, xsdt_addr)?;
    madt_addr.copy_to_guest(memory, xsdt_addr + size_of::<acpi_table_header>() as u64)?;

    let mut madt_header = acpi_table_madt {
        header: acpi_table_header {
            signature: signature!(ACPI_SIG_MADT; 4),
            length: madt_size as u32,
            revision: 6, // ACPI 6.5
            ..Default::default()
        },
        address: APIC_BASE,
        flags: 0,
    };
    let madt_io_apic = acpi_madt_io_apic {
        header: acpi_subtable_header {
            type_: acpi_madt_type_ACPI_MADT_TYPE_IO_APIC as u8,
            length: size_of::<acpi_madt_io_apic>() as u8,
        },
        id: 0,
        address: IOAPIC_ADDR,
        global_irq_base: 0,
        ..Default::default()
    };
    let madt_local_apics: Vec<_> = (0..num_cpus)
        .map(|id| {
            let id = id as u8;
            acpi_madt_local_apic {
                header: acpi_subtable_header {
                    type_: acpi_madt_type_ACPI_MADT_TYPE_LOCAL_APIC as u8,
                    length: size_of::<acpi_madt_local_apic>() as u8,
                },
                processor_id: id,
                id,
                lapic_flags: ACPI_MADT_ENABLED,
            }
        })
        .collect();
    madt_header.header.checksum = checksum!(madt_header.header, madt_io_apic, madt_local_apics);
    let mut addr = madt_addr;
    madt_header.copy_to_guest(memory, addr)?;
    addr += size_of::<acpi_table_madt>() as u64;
    madt_io_apic.copy_to_guest(memory, addr)?;
    addr += size_of::<acpi_madt_io_apic>() as u64;
    madt_local_apics.copy_to_guest(memory, addr)?;

    Ok(())
}

const GDT_BASE: u64 = 0x0500;
const IDT_BASE: u64 = 0x0530;
const PAGE_TABLE_ADDR: u64 = 0x8000;
const STACK_POINTER: u64 = 0x0008_0000;

pub const EBDA_START: u64 = 0x0009_fc00;
pub const RSDP_ADDR: u64 = 0x000e_0000;
pub const HIGH_MEMORY_START: u64 = 0x0010_0000;

const IOAPIC_ADDR: u32 = 0xfec0_0000;
const APIC_BASE: u32 = 0xfee0_0000;

// Follows the Linux x86 boot protocol
// https://www.kernel.org/doc/Documentation/x86/boot.txt

// __BOOT_CS (0x10)
const CODE_SEGMENT32: Segment = Segment {
    selector: 0x10,
    base: 0,
    limit: u32::MAX, // 4KiB granularity somehow doesn't work
    access: 0x9a,
    flags: 0xc,
};
const CODE_SEGMENT64: Segment = Segment {
    selector: 0x10,
    base: 0,
    limit: 0xfffff,
    access: 0x9a,
    flags: 0xa,
};

// __BOOT_DS (0x18)
const DATA_SEGMENT: Segment = Segment {
    selector: 0x18,
    base: 0,
    limit: 0xfffff,
    access: 0x96,
    flags: 0xc,
};

const TSS_SEGMENT: Segment = Segment {
    selector: 0x20,
    base: 0,
    limit: 0xfffff,
    access: 0x89,
    flags: 0x8,
};

const GDT32: &[u64] = &[
    0, // Null
    0, // Unused
    CODE_SEGMENT32.gdt_entry(),
    DATA_SEGMENT.gdt_entry(),
    TSS_SEGMENT.gdt_entry(),
];
const GDT64: &[u64] = &[
    0, // Null
    0, // Unused
    CODE_SEGMENT64.gdt_entry(),
    DATA_SEGMENT.gdt_entry(),
    TSS_SEGMENT.gdt_entry(),
    0, // Upper 32 bits of TSS base
];

const IDT32: &[u64] = &[0];
const IDT64: &[u64] = &[0, 0];

struct Segment {
    selector: u16,
    base: u64,
    limit: u32,
    access: u8,
    flags: u8,
}

impl Segment {
    const fn gdt_entry(&self) -> u64 {
        ((self.base & 0xffff) << 16)
            | ((self.base & 0xff_0000) << 16)
            | ((self.base & 0xff00_0000) << 32)
            | (self.limit as u64 & 0xffff)
            | ((self.limit as u64 & 0xf0000) << 32)
            | ((self.access as u64) << 40)
            | ((self.flags as u64) << 52)
    }

    const fn kvm_segment(&self) -> kvm_segment {
        kvm_segment {
            base: self.base,
            limit: self.limit,
            selector: self.selector,
            type_: self.access & 0xf,
            present: (self.access >> 7) & 1,
            dpl: (self.access >> 5) & 3,
            db: (self.flags >> 2) & 1,
            s: (self.access >> 4) & 1,
            l: (self.flags >> 1) & 1,
            g: (self.flags >> 3) & 1,
            avl: self.flags & 1,
            unusable: (!self.access >> 7) & 1,
            padding: 0,
        }
    }
}
