use kvm_bindings::{
    kvm_cpuid2, kvm_irq_level, kvm_pit_config, kvm_regs, kvm_sregs, kvm_userspace_memory_region,
    KVMIO,
};
use nix::{ioctl_read, ioctl_readwrite, ioctl_write_int_bad, ioctl_write_ptr, request_code_none};

// nix::ioctl_none! does not specify the third argument, which can result in
// EINVAL for some ioctls. This version specifies 0 instead.
macro_rules! ioctl_none {
    ($(#[$attr:meta])* $name:ident, $ioty:expr, $nr:expr) => (
        $(#[$attr])*
         pub unsafe fn $name(fd: nix::libc::c_int) -> nix::Result<nix::libc::c_int> {
            unsafe {
                nix::convert_ioctl_res!(nix::libc::ioctl(fd, nix::request_code_none!($ioty, $nr) as nix::sys::ioctl::ioctl_num_type, 0))
            }
        }
    )
}

// ioctl numbers can be found in
// https://git.kernel.org/pub/scm/linux/kernel/git/torvalds/linux.git/tree/include/uapi/linux/kvm.h

ioctl_none!(get_api_version, KVMIO, 0x00);
ioctl_write_int_bad!(create_vm, request_code_none!(KVMIO, 0x01));
ioctl_write_int_bad!(check_extension, request_code_none!(KVMIO, 0x03));
ioctl_none!(get_vcpu_mmap_size, KVMIO, 0x04);
ioctl_readwrite!(get_supported_cpuid, KVMIO, 0x05, kvm_cpuid2);
ioctl_write_ptr!(
    set_user_memory_region,
    KVMIO,
    0x46,
    kvm_userspace_memory_region
);
ioctl_write_int_bad!(create_vpu, request_code_none!(KVMIO, 0x41));
ioctl_none!(create_irqchip, KVMIO, 0x60);
ioctl_write_ptr!(irq_line, KVMIO, 0x61, kvm_irq_level);
ioctl_write_ptr!(create_pit2, KVMIO, 0x77, kvm_pit_config);
ioctl_none!(run, KVMIO, 0x80);
ioctl_write_ptr!(set_regs, KVMIO, 0x82, kvm_regs);
ioctl_read!(get_sregs, KVMIO, 0x83, kvm_sregs);
ioctl_write_ptr!(set_sregs, KVMIO, 0x84, kvm_sregs);
ioctl_write_ptr!(set_cpuid2, KVMIO, 0x90, kvm_cpuid2);
