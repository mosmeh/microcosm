use crate::{load::BootProtocol, memory::CopyToGuest, Result};
use std::mem::size_of;
use sys::kvm_bindings::{kvm_regs, kvm_segment, kvm_sregs};
use zerocopy::AsBytes;

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
    }

    pub fn configure_regs(&self, regs: &mut kvm_regs) {
        regs.rflags = 0x2;
        regs.rip = self.entry_addr;
        regs.rsp = STACK_POINTER;
        self.protocol.configure_regs(regs, self.params_addr);
    }
}

const GDT_BASE: u64 = 0x0500;
const IDT_BASE: u64 = 0x0530;
const PAGE_TABLE_ADDR: u64 = 0x8000;
const STACK_POINTER: u64 = 0x0008_0000;

pub const EBDA_START: u64 = 0x0009_fc00;
pub const HIGH_MEMORY_START: u64 = 0x0010_0000;

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
