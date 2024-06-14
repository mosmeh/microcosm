use crate::{
    boot::{self, Bootable},
    device::{self, PortIoDevice},
    kvm::{Vcpu, Vm},
    memory::Mmapped,
    Hypervisor, KernelParams, Result,
};
use std::{
    ffi::CString,
    num::NonZeroUsize,
    path::PathBuf,
    sync::{Arc, Mutex},
};
use sys::kvm_bindings::{
    self, kvm_pit_config, kvm_regs, kvm_run, kvm_userspace_memory_region, CpuId, KVM_EXIT_HLT,
    KVM_EXIT_INTERNAL_ERROR, KVM_EXIT_IO, KVM_EXIT_IO_IN, KVM_EXIT_IO_OUT, KVM_EXIT_SHUTDOWN,
};

pub struct GuestBuilder<'a> {
    hypervisor: &'a Hypervisor,
    kernel_path: PathBuf,
    num_cpus: NonZeroUsize,
    memory_size: NonZeroUsize,
    kernel_params: KernelParams,
}

impl<'a> GuestBuilder<'a> {
    pub(crate) fn new(hypervisor: &'a Hypervisor, kernel_path: PathBuf) -> Self {
        Self {
            hypervisor,
            kernel_path,
            num_cpus: NonZeroUsize::new(1).unwrap(),
            memory_size: NonZeroUsize::new(64 * 1024 * 1024).unwrap(),
            kernel_params: KernelParams::default(),
        }
    }

    #[must_use]
    pub fn num_cpus(mut self, num_cpus: NonZeroUsize) -> Self {
        self.num_cpus = num_cpus;
        self
    }

    #[must_use]
    pub fn memory_size(mut self, bytes: NonZeroUsize) -> Self {
        self.memory_size = bytes;
        self
    }

    #[must_use]
    pub fn cmdline(mut self, cmdline: impl Into<CString>) -> Self {
        self.kernel_params.cmdline = Some(cmdline.into());
        self
    }

    #[must_use]
    pub fn initrd(mut self, path: impl Into<PathBuf>) -> Self {
        self.kernel_params.initrd_path = Some(path.into());
        self
    }

    #[must_use]
    pub fn add_module(mut self, path: impl Into<PathBuf>) -> Self {
        self.kernel_params.module_paths.push(path.into());
        self
    }

    pub fn build(self) -> Result<Guest> {
        let kernel = std::fs::read(&self.kernel_path)?;

        let mut mmapped_memory = Mmapped::new_anonymous(self.memory_size)?;
        let memory = mmapped_memory.as_mut_slice();

        let bootable = Bootable::load(memory, &kernel, self.kernel_params)?;
        eprintln!("Protocol: {:?}", bootable.protocol);
        eprintln!("Entry: {:#x}", bootable.entry_addr);
        bootable.configure_memory(memory)?;
        boot::configure_acpi(memory, self.num_cpus.get())?;

        let vm = Vm::new(self.hypervisor.kvm.clone())?;
        vm.set_user_memory_region(&kvm_userspace_memory_region {
            slot: 0,
            flags: 0,
            guest_phys_addr: 0,
            memory_size: memory.len() as u64,
            userspace_addr: memory.as_ptr() as u64,
        })?;
        vm.create_irqchip()?;
        vm.create_pit2(&kvm_pit_config::default())?;

        Ok(Guest {
            vm: Arc::new(vm),
            num_cpus: self.num_cpus,
            port_io_hub: PortIoHub::default(),
            supported_cpuid: self.hypervisor.supported_cpuid.clone(),
            vcpu_mmap_size: self.hypervisor.vcpu_mmap_size,
            bootable,
            _memory: mmapped_memory,
        })
    }
}

type PortIoHub = device::PortIoHub<Arc<Mutex<dyn PortIoDevice + Send>>>;

pub struct Guest {
    vm: Arc<Vm>,
    num_cpus: NonZeroUsize,
    port_io_hub: PortIoHub,
    supported_cpuid: CpuId,
    vcpu_mmap_size: NonZeroUsize,
    bootable: Bootable,
    _memory: Mmapped<u8>,
}

impl Guest {
    pub fn add_device<I, D>(&mut self, device: I) -> Result<()>
    where
        I: Into<Arc<Mutex<D>>>,
        D: PortIoDevice + Send + 'static,
    {
        self.port_io_hub.add_device(device.into())
    }

    pub fn irq(&self) -> Irq {
        Irq {
            vm: self.vm.clone(),
        }
    }

    pub fn run(self) -> Result<()> {
        let cpu = Cpu {
            vm: self.vm,
            port_io_hub: Arc::new(Mutex::new(self.port_io_hub)),
            cpuid: self.supported_cpuid,
            vcpu_mmap_size: self.vcpu_mmap_size,
            bootable: self.bootable,
        };
        let cpus: Vec<_> = (0..self.num_cpus.get())
            .map(|id| {
                let cpu = cpu.clone();
                std::thread::Builder::new()
                    .name(format!("cpu{id}"))
                    .spawn(move || cpu.run(id as u32))
            })
            .collect();
        for cpu in cpus {
            cpu?.join().unwrap()?;
        }
        Ok(())
    }
}

#[derive(Clone)]
struct Cpu {
    vm: Arc<Vm>,
    port_io_hub: Arc<Mutex<PortIoHub>>,
    cpuid: CpuId,
    vcpu_mmap_size: NonZeroUsize,
    bootable: Bootable,
}

impl Cpu {
    fn run(mut self, id: u32) -> Result<()> {
        let vcpu = Vcpu::new(self.vm, id)?;

        for entry in self.cpuid.as_mut_slice() {
            match entry.function {
                0x1 => {
                    // Set local APIC ID
                    entry.ebx &= !(0xff << 24);
                    entry.ebx |= id << 24;

                    if entry.index == 0 {
                        // Set X86_FEATURE_HYPERVISOR
                        entry.ecx |= 1 << 31;
                    }
                }
                0xb => {
                    // Set x2APIC ID
                    entry.edx = id;
                }
                0x8000_0001 if self.bootable.protocol.is_32bit() => {
                    entry.ecx &= !(1 << 29); // Disable 64-bit mode
                }
                _ => {}
            }
        }
        vcpu.set_cpuid(&self.cpuid)?;

        let mut sregs = vcpu.sregs()?;
        self.bootable.configure_sregs(&mut sregs);
        vcpu.set_sregs(&sregs)?;

        let mut regs = kvm_regs::default();
        self.bootable.configure_regs(&mut regs);
        vcpu.set_regs(&regs)?;

        let run = Mmapped::<kvm_run>::new_file(&vcpu, self.vcpu_mmap_size)?;

        macro_rules! eprintln_kvm_consts {
            ($x:expr => $s:expr; $($v:ident,)*) => {
                match $x {
                    $(kvm_bindings::$v => eprintln!(stringify!($v)),)*
                    _ => eprintln!(concat!("Unknown ", $s, " {}"), $x),
                }
            }
        }

        loop {
            match unsafe { vcpu.run() } {
                Ok(()) => {}
                Err(nix::Error::EAGAIN | nix::Error::EINTR) => continue,
                Err(e) => return Err(e.into()),
            }
            let exit_reason = run.as_ref().exit_reason;
            match exit_reason {
                KVM_EXIT_IO => {
                    let io = unsafe { run.as_ref().__bindgen_anon_1.io };
                    let ptr = run.as_ptr().cast::<u8>();
                    let ptr = unsafe { ptr.offset(io.data_offset as isize) };
                    let len = io.size as usize * io.count as usize;
                    let data = unsafe { std::slice::from_raw_parts_mut(ptr, len) };
                    let mut port_io_hub = self.port_io_hub.lock().unwrap();
                    match io.direction.into() {
                        KVM_EXIT_IO_IN => port_io_hub.read(io.port, data)?,
                        KVM_EXIT_IO_OUT => port_io_hub.write(io.port, data)?,
                        _ => eprintln!("Unknown IO direction {}", io.direction),
                    }
                }
                KVM_EXIT_HLT | KVM_EXIT_SHUTDOWN => break,
                KVM_EXIT_INTERNAL_ERROR => {
                    let internal = unsafe { run.as_ref().__bindgen_anon_1.internal };
                    eprintln_kvm_consts! {
                        internal.suberror => "internal error";
                        KVM_INTERNAL_ERROR_EMULATION,
                        KVM_INTERNAL_ERROR_SIMUL_EX,
                        KVM_INTERNAL_ERROR_DELIVERY_EV,
                        KVM_INTERNAL_ERROR_UNEXPECTED_EXIT_REASON,
                    }
                    break;
                }
                reason => {
                    eprintln_kvm_consts! {
                        reason => "exit reason";
                        KVM_EXIT_UNKNOWN,
                        KVM_EXIT_EXCEPTION,
                        KVM_EXIT_HYPERCALL,
                        KVM_EXIT_DEBUG,
                        KVM_EXIT_MMIO,
                        KVM_EXIT_IRQ_WINDOW_OPEN,
                        KVM_EXIT_FAIL_ENTRY,
                        KVM_EXIT_INTR,
                        KVM_EXIT_SET_TPR,
                        KVM_EXIT_TPR_ACCESS,
                        KVM_EXIT_S390_SIEIC,
                        KVM_EXIT_S390_RESET,
                        KVM_EXIT_DCR,
                        KVM_EXIT_NMI,
                        KVM_EXIT_OSI,
                        KVM_EXIT_PAPR_HCALL,
                        KVM_EXIT_S390_UCONTROL,
                        KVM_EXIT_WATCHDOG,
                        KVM_EXIT_S390_TSCH,
                        KVM_EXIT_EPR,
                        KVM_EXIT_SYSTEM_EVENT,
                        KVM_EXIT_S390_STSI,
                        KVM_EXIT_IOAPIC_EOI,
                        KVM_EXIT_HYPERV,
                        KVM_EXIT_ARM_NISV,
                        KVM_EXIT_X86_RDMSR,
                        KVM_EXIT_X86_WRMSR,
                        KVM_EXIT_DIRTY_RING_FULL,
                        KVM_EXIT_AP_RESET_HOLD,
                        KVM_EXIT_X86_BUS_LOCK,
                        KVM_EXIT_XEN,
                        KVM_EXIT_RISCV_SBI,
                        KVM_EXIT_RISCV_CSR,
                        KVM_EXIT_NOTIFY,
                    }
                }
            }
        }
        Ok(())
    }
}

#[derive(Clone)]
pub struct Irq {
    vm: Arc<Vm>,
}

impl Irq {
    pub fn set_level(&self, irq: u8, level: bool) -> nix::Result<()> {
        self.vm.set_irq_line(irq, level)
    }
}
