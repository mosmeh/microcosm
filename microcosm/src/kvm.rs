use crate::{Error, Result};
use nix::{
    fcntl::{open, OFlag},
    libc::c_int,
    sys::stat::Mode,
};
use std::{
    fs::File,
    num::NonZeroUsize,
    os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd},
    sync::Arc,
};
use sys::{
    kvm,
    kvm_bindings::{
        self, kvm_irq_level, kvm_pit_config, kvm_regs, kvm_sregs, kvm_userspace_memory_region,
        CpuId, KVM_MAX_CPUID_ENTRIES,
    },
};

pub struct Kvm {
    file: File,
}

impl Kvm {
    pub fn new() -> nix::Result<Self> {
        let fd = open("/dev/kvm", OFlag::O_RDWR | OFlag::O_CLOEXEC, Mode::empty())?;
        let file = unsafe { File::from_raw_fd(fd) };
        Ok(Self { file })
    }

    pub fn api_version(&self) -> nix::Result<c_int> {
        unsafe { kvm::get_api_version(self.file.as_raw_fd()) }
    }

    pub fn check_extension(&self, cap: c_int) -> nix::Result<c_int> {
        unsafe { kvm::check_extension(self.file.as_raw_fd(), cap) }
    }

    pub fn supported_cpuid(&self) -> nix::Result<CpuId> {
        let mut cpuid = CpuId::new(KVM_MAX_CPUID_ENTRIES).unwrap();
        unsafe { kvm::get_supported_cpuid(self.file.as_raw_fd(), cpuid.as_mut_fam_struct_ptr())? };
        Ok(cpuid)
    }

    pub fn vcpu_mmap_size(&self) -> Result<NonZeroUsize> {
        let size = unsafe { kvm::get_vcpu_mmap_size(self.file.as_raw_fd())? };
        let size: usize = size
            .try_into()
            .map_err(|_| Error::InvalidVcpuMmapSize(size.to_string()))?;
        NonZeroUsize::new(size).ok_or_else(|| Error::InvalidVcpuMmapSize(size.to_string()))
    }
}

pub struct Vm {
    file: File,
    _kvm: Arc<Kvm>,
}

impl Vm {
    pub fn new(kvm: Arc<Kvm>) -> nix::Result<Self> {
        let fd = unsafe { kvm::create_vm(kvm.file.as_raw_fd(), 0)? };
        let file = unsafe { File::from_raw_fd(fd) };
        Ok(Self { file, _kvm: kvm })
    }

    pub fn set_user_memory_region(
        &self,
        userspace_memory_region: &kvm_userspace_memory_region,
    ) -> nix::Result<()> {
        unsafe { kvm::set_user_memory_region(self.file.as_raw_fd(), userspace_memory_region)? };
        Ok(())
    }

    pub fn create_irqchip(&self) -> nix::Result<()> {
        unsafe { kvm::create_irqchip(self.file.as_raw_fd())? };
        Ok(())
    }

    pub fn create_pit2(&self, pit_config: &kvm_pit_config) -> nix::Result<()> {
        unsafe { kvm::create_pit2(self.file.as_raw_fd(), pit_config)? };
        Ok(())
    }

    pub fn set_irq_line(&self, irq: u8, level: bool) -> nix::Result<()> {
        let irq_level = kvm_irq_level {
            __bindgen_anon_1: kvm_bindings::kvm_irq_level__bindgen_ty_1 { irq: irq.into() },
            level: level.into(),
        };
        unsafe { kvm::irq_line(self.file.as_raw_fd(), &irq_level)? };
        Ok(())
    }
}

pub struct Vcpu {
    file: File,
    _vm: Arc<Vm>,
}

impl Vcpu {
    pub fn new(vm: Arc<Vm>, id: u32) -> nix::Result<Self> {
        let fd = unsafe { kvm::create_vpu(vm.file.as_raw_fd(), id as c_int)? };
        let file = unsafe { File::from_raw_fd(fd) };
        Ok(Self { file, _vm: vm })
    }

    pub fn set_cpuid(&self, cpuid: &CpuId) -> nix::Result<()> {
        unsafe { kvm::set_cpuid2(self.file.as_raw_fd(), cpuid.as_fam_struct_ptr()) }?;
        Ok(())
    }

    pub fn sregs(&self) -> nix::Result<kvm_sregs> {
        let mut sregs = kvm_sregs::default();
        unsafe { kvm::get_sregs(self.file.as_raw_fd(), &mut sregs)? };
        Ok(sregs)
    }

    pub fn set_sregs(&self, sregs: &kvm_sregs) -> nix::Result<()> {
        unsafe { kvm::set_sregs(self.file.as_raw_fd(), sregs)? };
        Ok(())
    }

    pub fn set_regs(&self, regs: &kvm_regs) -> nix::Result<()> {
        unsafe { kvm::set_regs(self.file.as_raw_fd(), regs)? };
        Ok(())
    }

    pub unsafe fn run(&self) -> nix::Result<()> {
        kvm::run(self.file.as_raw_fd())?;
        Ok(())
    }
}

impl AsFd for Vcpu {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.file.as_fd()
    }
}
