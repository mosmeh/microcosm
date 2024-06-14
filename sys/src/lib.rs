#![allow(
    clippy::missing_safety_doc,
    non_camel_case_types,
    non_snake_case,
    non_upper_case_globals
)]

pub mod acpi;
pub mod bootparam;
pub mod e820;
pub mod elf;
pub mod elfnote;
pub mod kvm;
pub mod multiboot;
pub mod serial_reg;
pub mod start_info;

pub use kvm_bindings;
