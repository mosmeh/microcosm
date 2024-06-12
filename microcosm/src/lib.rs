pub mod device;

mod boot;
mod guest;
mod kvm;
mod load;
mod memory;

pub use guest::{Guest, GuestBuilder};

use kvm::Kvm;
use std::{ffi::CString, num::NonZeroUsize, path::PathBuf, sync::Arc};
use sys::kvm_bindings::{self, kvm_run, CpuId, KVM_API_VERSION};

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("KVM API version mismatch: expected {expected}, got {actual}")]
    KvmApiVersionMismatch { actual: u32, expected: u32 },

    #[error("KVM extension not supported: {0}")]
    KvmExtensionNotSupported(&'static str),

    #[error("Invalid VCPU mmap size {0}")]
    InvalidVcpuMmapSize(String),

    #[error("Invalid or unknown kernel image format")]
    InvalidKernelImageFormat,

    #[error("Kernel command line too long: {len} > {max_len}")]
    CmdlineTooLong { len: usize, max_len: usize },

    #[error("initrd too large: {size} > {max_size}")]
    InitrdTooLarge { size: usize, max_size: usize },

    #[error("Attempted to add device with overlapping port or address range")]
    DeviceRangeOverlap,

    #[error("Out of guest memory")]
    OutOfGuestMemory,

    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    Syscall(#[from] nix::Error),
}

#[derive(Clone, Default)]
struct KernelParams {
    cmdline: Option<CString>,
    initrd_path: Option<PathBuf>,
    module_paths: Vec<PathBuf>,
}

pub struct Hypervisor {
    kvm: Arc<Kvm>,
    supported_cpuid: CpuId,
    vcpu_mmap_size: NonZeroUsize,
}

impl Hypervisor {
    pub fn new() -> Result<Self> {
        let kvm = Kvm::new()?;
        let api_version = kvm.api_version()? as u32;
        if api_version != KVM_API_VERSION {
            return Err(Error::KvmApiVersionMismatch {
                actual: api_version,
                expected: KVM_API_VERSION,
            });
        }

        macro_rules! ensure_extensions {
            ($($cap:ident,)*) => {
                $(
                     if kvm.check_extension(kvm_bindings::$cap as nix::libc::c_int)? <= 0 {
						return Err(Error::KvmExtensionNotSupported(stringify!($cap)));
					}
                )*
            };
        }

        ensure_extensions! {
            KVM_CAP_IRQCHIP,
            KVM_CAP_USER_MEMORY,
            KVM_CAP_EXT_CPUID,
            KVM_CAP_PIT2,
        };

        let supported_cpuid = kvm.supported_cpuid()?;
        let vcpu_mmap_size = kvm.vcpu_mmap_size()?;
        if vcpu_mmap_size.get() < std::mem::size_of::<kvm_run>() {
            return Err(Error::InvalidVcpuMmapSize(vcpu_mmap_size.to_string()));
        }

        Ok(Self {
            kvm: Arc::new(kvm),
            supported_cpuid,
            vcpu_mmap_size,
        })
    }

    #[must_use]
    pub fn guest(&self, kernel_path: impl Into<PathBuf>) -> GuestBuilder {
        GuestBuilder::new(self, kernel_path.into())
    }
}
