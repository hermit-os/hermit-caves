use std::mem;
use std::ptr;
use std::fs::File;
use std::os::unix::io::{FromRawFd, RawFd};
use std::ptr::{read_volatile, write_volatile};
use std::thread;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::sync::atomic::Ordering;
use std::rc::Rc;

use nix::sys::signal;
use nix::sys::pthread;
use nix::errno::Errno;
use nix;

use memmap::{MmapMut, MmapOptions};

use hermit::utils::MemoryMapMut;
use hermit::uhyve;
use hermit::uhyve::kvm::*;
use hermit::error::*;
use super::proto;
use super::vm::{KVMExtensions, ControlData};
use super::network::NetworkInterface;
use super::utils;

pub const CPUID_FUNC_PERFMON: u32 = 0x0A;

/* x86-64 specific MSRs */
pub const MSR_EFER:		            u32 = 0xc0000080; /* extended feature register */
pub const MSR_STAR:		            u32 = 0xc0000081; /* legacy mode SYSCALL target */
pub const MSR_LSTAR:	        	u32 = 0xc0000082; /* long mode SYSCALL target */
pub const MSR_CSTAR:		        u32 = 0xc0000083; /* compat mode SYSCALL target */
pub const MSR_SYSCALL_MASK:	        u32 = 0xc0000084; /* EFLAGS mask for syscall */
pub const MSR_FS_BASE:		        u32 = 0xc0000100; /* 64bit FS base */
pub const MSR_GS_BASE:		        u32 = 0xc0000101; /* 64bit GS base */
pub const MSR_KERNEL_GS_BASE:	    u32 = 0xc0000102; /* SwapGS GS shadow */
pub const MSR_TSC_AUX:	        	u32 = 0xc0000103; /* Auxiliary TSC */

pub const MSR_IA32_CR_PAT:          u32 = 0x00000277;
pub const MSR_PEBS_FRONTEND:        u32 = 0x000003f7;

pub const MSR_IA32_POWER_CTL:       u32 = 0x000001fc;

pub const MSR_IA32_MC0_CTL:         u32 = 0x00000400;
pub const MSR_IA32_MC0_STATUS:      u32 = 0x00000401;
pub const MSR_IA32_MC0_ADDR:        u32 = 0x00000402;
pub const MSR_IA32_MC0_MISC:        u32 = 0x00000403;

pub const MSR_IA32_SYSENTER_CS:     u32 = 0x00000174;
pub const MSR_IA32_SYSENTER_ESP:    u32 = 0x00000175;
pub const MSR_IA32_SYSENTER_EIP:    u32 = 0x00000176;

pub const MSR_IA32_APICBASE:        u32 = 0x0000001b;
pub const MSR_IA32_APICBASE_BSP:    u32 = 1<<8;
pub const MSR_IA32_APICBASE_ENABLE: u32 = 1<<11;
pub const MSR_IA32_APICBASE_BASE:   u32 = 0xfffff<<12;

pub const MSR_IA32_MISC_ENABLE:     u32 = 0x000001a0;
pub const MSR_IA32_TSC:             u32 = 0x00000010;

pub const IOAPIC_DEFAULT_BASE:  u32 = 0xfec00000;
pub const APIC_DEFAULT_BASE:    u32 = 0xfee00000;

#[repr(C)]
#[derive(Default)]
pub struct vcpu_state {
	pub msr_data: kvm_msr_data,
	pub regs: kvm_regs,
	pub sregs: kvm_sregs,
	pub fpu: kvm_fpu,
	pub lapic: kvm_lapic_state,
	pub xsave: kvm_xsave,
	pub xcrs: kvm_xcrs,
	pub events: kvm_vcpu_events,
	pub mp_state: kvm_mp_state,
}

pub enum ExitCode {
    Cause(Result<i32>),
    Innocent
}

pub struct SharedState {
    run_mem: MmapMut,
    mboot: *mut u8,
    guest_mem: *mut u8,
    control: Arc<ControlData>,
}

pub struct VirtualCPU {
    kvm: Rc<uhyve::KVM>,
    id: u32,
    vcpu_fd: RawFd,
    state: Option<SharedState>,
    extensions: KVMExtensions
}

extern "C" fn empty_handler(_: i32) {}

impl VirtualCPU {
    pub fn new(kvm: Rc<uhyve::KVM>, vcpu_fd: RawFd, id: u32, mmap_size: usize, mem: &mut MemoryMapMut,
        mboot: *mut u8, control: Arc<ControlData>, extensions: KVMExtensions) -> Result<VirtualCPU> {
        debug!("New virtual CPU with id {} and FD {}", id, vcpu_fd);

        let file = unsafe { File::from_raw_fd(vcpu_fd) };
        let mut run_mem = unsafe { MmapOptions::new().len(mmap_size).map_mut(&file) }
            .map_err(|_| Error::NotEnoughMemory)?;
      
        // forget the file, we don't want to close the file descriptor
        mem::forget(file);

        unsafe {
            let mut run = *(run_mem.as_mut_ptr() as *mut kvm_run);
            run.apic_base = APIC_DEFAULT_BASE as u64;
        }

        let state = SharedState {
            run_mem: run_mem,
            mboot: mboot,
            guest_mem: mem.as_mut_ptr(),
            control: control,
        };

        let cpu = VirtualCPU {
            kvm: kvm,
            vcpu_fd: vcpu_fd,
            id: id,
            state: Some(state),
            extensions: extensions
        };

        Ok(cpu)
    }

    pub fn get_id(&self) -> u32 {
        self.id
    }

    pub fn init(&self, entry: u64) -> Result<()> {
        debug!("Set the CPUID");
        
        self.setup_cpuid()?;

        debug!("Set MP state");

        self.set_mp_state(kvm_mp_state { mp_state: KVM_MP_STATE_RUNNABLE })?;

        let mut msr_data = kvm_msr_data { info: kvm_msrs::default(), entries: [kvm_msr_entry::default(); 25] };
        msr_data.entries[0].index = MSR_IA32_MISC_ENABLE;
        msr_data.entries[0].data = 1;
        msr_data.info.nmsrs = 1;
        self.set_msrs(&mut msr_data)?;

        debug!("Initialize the register of {} with start address {:?}", self.id, entry);

        let mut regs = kvm_regs::default();
        regs.rip = entry;
        regs.rflags = 0x2;
        self.set_regs(regs)?;

        Ok(())
    }

    pub fn restore_cpu_state(&self, cpu_state: &mut vcpu_state) -> Result<()> {
        cpu_state.mp_state.mp_state = KVM_MP_STATE_RUNNABLE;

        //run.apic_base = APIC_DEFAULT_BASE as u64;
        self.setup_cpuid()?;

        self.set_sregs(cpu_state.sregs)?;
        self.set_regs(cpu_state.regs)?;
        self.set_msrs(&mut cpu_state.msr_data)?;
        self.set_xcrs(cpu_state.xcrs)?;
        self.set_mp_state(cpu_state.mp_state)?;
        self.set_lapic(cpu_state.lapic)?;
        self.set_fpu(cpu_state.fpu)?;
        self.set_xsave(cpu_state.xsave)?;
        self.set_vcpu_events(cpu_state.events)?;

        Ok(())
    }

    pub fn save_cpu_state(&self) -> Result<vcpu_state> {
        let mut cpu_state = vcpu_state::default();

        /* define the list of required MSRs */
        cpu_state.msr_data.entries[0].index = MSR_IA32_APICBASE;
        cpu_state.msr_data.entries[1].index = MSR_IA32_SYSENTER_CS;
        cpu_state.msr_data.entries[2].index = MSR_IA32_SYSENTER_ESP;
        cpu_state.msr_data.entries[3].index = MSR_IA32_SYSENTER_EIP;
        cpu_state.msr_data.entries[4].index = MSR_IA32_CR_PAT;
        cpu_state.msr_data.entries[5].index = MSR_IA32_MISC_ENABLE;
        cpu_state.msr_data.entries[6].index = MSR_IA32_TSC;
        cpu_state.msr_data.entries[7].index = MSR_CSTAR;
        cpu_state.msr_data.entries[8].index = MSR_STAR;
        cpu_state.msr_data.entries[9].index = MSR_EFER;
        cpu_state.msr_data.entries[10].index = MSR_LSTAR;
        cpu_state.msr_data.entries[11].index = MSR_GS_BASE;
        cpu_state.msr_data.entries[12].index = MSR_FS_BASE;
        cpu_state.msr_data.entries[13].index = MSR_KERNEL_GS_BASE;

        cpu_state.msr_data.info.nmsrs = 14;

        // run.apic_base = APIC_DEFAULT_BASE as u64;

        cpu_state.sregs = self.get_sregs()?;
        cpu_state.regs = self.get_regs()?;
        self.get_msrs(&mut cpu_state.msr_data)?;
        cpu_state.xcrs = self.get_xcrs()?;
        cpu_state.lapic = self.get_lapic()?;
        cpu_state.fpu = self.get_fpu()?;
        cpu_state.xsave = self.get_xsave()?;
        cpu_state.events = self.get_vcpu_events()?;
        cpu_state.mp_state = self.get_mp_state()?;

        Ok(cpu_state)
    }

    pub fn print_registers(id: u32, vcpu_fd: RawFd) -> Result<()> {
        utils::show_registers(id, &VirtualCPU::get_regs_fd(vcpu_fd)?, &VirtualCPU::get_sregs_fd(vcpu_fd)?);
        Ok(())
    }

    fn get_sregs_fd(vcpu_fd: RawFd) -> Result<kvm_sregs> {
        let mut sregs = kvm_sregs::default();
        unsafe {
            uhyve::ioctl::get_sregs(vcpu_fd, (&mut sregs) as *mut kvm_sregs)
                .map_err(|_| Error::IOCTL(NameIOCTL::GetSRegs))?;
        }

        Ok(sregs)
    }

    pub fn get_sregs(&self) -> Result<kvm_sregs> {
        VirtualCPU::get_sregs_fd(self.vcpu_fd)
    }

    pub fn set_sregs(&self, mut sregs: kvm_sregs) -> Result<()> {
        unsafe {
            uhyve::ioctl::set_sregs(self.vcpu_fd, (&mut sregs) as *mut kvm_sregs)
                .map_err(|_| Error::IOCTL(NameIOCTL::SetSRegs))?;
        }

        Ok(())
    }

    fn get_regs_fd(vcpu_fd: RawFd) -> Result<kvm_regs> {
        let mut regs = kvm_regs::default();
        unsafe {
            uhyve::ioctl::get_regs(vcpu_fd, (&mut regs) as *mut kvm_regs)
                .map_err(|_| Error::IOCTL(NameIOCTL::GetRegs))?;
        }

        Ok(regs)
    }

    fn get_regs(&self) -> Result<kvm_regs> {
        VirtualCPU::get_regs_fd(self.vcpu_fd)
    }

    fn set_regs(&self, mut regs: kvm_regs) -> Result<()> {
        unsafe {
            uhyve::ioctl::set_regs(self.vcpu_fd, (&mut regs) as *mut kvm_regs)
                .map_err(|_| Error::IOCTL(NameIOCTL::SetSRegs))?;
        }

        Ok(())
    }

    fn set_cpuid2(&self, mut cpuid: kvm_cpuid2_data) -> Result<()> {
        unsafe {
            uhyve::ioctl::set_cpuid2(self.vcpu_fd, (&mut cpuid.header) as *mut kvm_cpuid2)
                .map_err(|_| Error::IOCTL(NameIOCTL::SetCpuID2))?;
        }

        Ok(())
    }

    fn get_mp_state(&self) -> Result<kvm_mp_state> {
        let mut data = kvm_mp_state::default();
        unsafe {
            uhyve::ioctl::get_mp_state(self.vcpu_fd, (&mut data) as *mut kvm_mp_state)
                .map_err(|_| Error::IOCTL(NameIOCTL::GetMPState))?;
        }

        Ok(data)
    }
    
    fn set_mp_state(&self, mp_state: kvm_mp_state) -> Result<()> {
        unsafe {
            uhyve::ioctl::set_mp_state(self.vcpu_fd, (&mp_state) as *const kvm_mp_state)
                .map_err(|_| Error::IOCTL(NameIOCTL::SetMPState)).map(|_| ())
        }
    }

    fn get_msrs(&self, msr: &mut kvm_msr_data) -> Result<()> {
        unsafe {
            uhyve::ioctl::get_msrs(self.vcpu_fd, (&mut msr.info) as *mut kvm_msrs)
                .map_err(|_| Error::IOCTL(NameIOCTL::GetMSRS))?;
        }

        Ok(())
    }

    pub fn set_msrs(&self, msr: &mut kvm_msr_data) -> Result<()> {
        unsafe {
            uhyve::ioctl::set_msrs(self.vcpu_fd, (&mut msr.info) as *mut kvm_msrs)
                .map_err(|_| Error::IOCTL(NameIOCTL::SetMSRS))?;
        }

        Ok(())
    }

    fn get_fpu(&self) -> Result<kvm_fpu> {
        let mut data = kvm_fpu::default();
        unsafe {
            uhyve::ioctl::get_fpu(self.vcpu_fd, (&mut data) as *mut kvm_fpu)
                .map_err(|_| Error::IOCTL(NameIOCTL::GetFPU))?;
        }

        Ok(data)
    }

    fn set_fpu(&self, mut data: kvm_fpu) -> Result<()> {
        unsafe {
            uhyve::ioctl::set_fpu(self.vcpu_fd, (&mut data) as *mut kvm_fpu)
                .map_err(|_| Error::IOCTL(NameIOCTL::SetFPU))?;
        }

        Ok(())
    }

    fn get_lapic(&self) -> Result<kvm_lapic_state> {
        let mut data = kvm_lapic_state::default();
        unsafe {
            uhyve::ioctl::get_lapic(self.vcpu_fd, (&mut data) as *mut kvm_lapic_state)
                .map_err(|_| Error::IOCTL(NameIOCTL::GetLapic))?;
        }

        Ok(data)
    }

    fn set_lapic(&self, mut data: kvm_lapic_state) -> Result<()> {
        unsafe {
            uhyve::ioctl::set_lapic(self.vcpu_fd, (&mut data) as *mut kvm_lapic_state)
                .map_err(|_| Error::IOCTL(NameIOCTL::SetLapic))?;
        }

        Ok(())
    }

    fn get_vcpu_events(&self) -> Result<kvm_vcpu_events> {
        let mut data = kvm_vcpu_events::default();
        unsafe {
            uhyve::ioctl::get_vcpu_events(self.vcpu_fd, (&mut data) as *mut kvm_vcpu_events)
                .map_err(|_| Error::IOCTL(NameIOCTL::GetVCPUEvents))?;
        }

        Ok(data)
    }

    fn set_vcpu_events(&self, mut data: kvm_vcpu_events) -> Result<()> {
        unsafe {
            uhyve::ioctl::set_vcpu_events(self.vcpu_fd, (&mut data) as *mut kvm_vcpu_events)
                .map_err(|_| Error::IOCTL(NameIOCTL::SetVCPUEvents))?;
        }

        Ok(())
    }

    fn get_xsave(&self) -> Result<kvm_xsave> {
        let mut data = kvm_xsave::default();
        unsafe {
            uhyve::ioctl::get_xsave(self.vcpu_fd, (&mut data) as *mut kvm_xsave)
                .map_err(|_| Error::IOCTL(NameIOCTL::GetXSave))?;
        }

        Ok(data)
    }

    fn set_xsave(&self, mut data: kvm_xsave) -> Result<()> {
        unsafe {
            uhyve::ioctl::set_xsave(self.vcpu_fd, (&mut data) as *mut kvm_xsave)
                .map_err(|_| Error::IOCTL(NameIOCTL::SetXSave))?;
        }

        Ok(())
    }

    fn get_xcrs(&self) -> Result<kvm_xcrs> {
        let mut data = kvm_xcrs::default();
        unsafe {
            uhyve::ioctl::get_xcrs(self.vcpu_fd, (&mut data) as *mut kvm_xcrs)
                .map_err(|_| Error::IOCTL(NameIOCTL::GetXCRS))?;
        }

        Ok(data)
    }

    fn set_xcrs(&self, mut data: kvm_xcrs) -> Result<()> {
        unsafe {
            uhyve::ioctl::set_xcrs(self.vcpu_fd, (&mut data) as *mut kvm_xcrs)
                .map_err(|_| Error::IOCTL(NameIOCTL::SetXCRS))?;
        }

        Ok(())
    }

    fn set_signal_mask_fd(vcpu_fd: i32, mut data: kvm_signal_mask_data) -> Result<()> {
        unsafe {
            uhyve::ioctl::set_signal_mask(vcpu_fd, (&mut data.info) as *mut kvm_signal_mask)
                .map_err(|_| Error::IOCTL(NameIOCTL::SetSignalMask))?;
        }

        Ok(())
    }

    pub fn single_run(fd: RawFd, id: u32, state: &SharedState, net_if: &Arc<Option<NetworkInterface>>) -> Result<proto::Return> {
        let ret = unsafe { uhyve::ioctl::run(fd, ptr::null_mut()) };

        //debug!("Single Run CPU {}", id);

        if let Err(e) = ret {
            return match e {
                nix::Error::Sys(errno) => match errno {
                    Errno::EINTR => Ok(proto::Return::Interrupt),
                    Errno::EFAULT => {
                        let regs = VirtualCPU::get_regs_fd(fd)?;
                        Err(Error::TranslationFault(regs.rip))
                    },
                    _ => Err(Error::IOCTL(NameIOCTL::Run))
                },
                _ => Err(Error::IOCTL(NameIOCTL::Run))
            }
        }

        unsafe {
            let res = proto::Syscall::from_mem(state.run_mem.as_ptr(), state.guest_mem)?.run(state.guest_mem, net_if);
            if let Err(e) = &res {
                match e {
                    Error::KVMDebug => { let _ = VirtualCPU::print_registers(id, fd); },
                    _ => {}
                };
            }

            res
        }
    }

    pub fn run_vcpu(state: SharedState, id: u32, fd: RawFd, net_if: Arc<Option<NetworkInterface>>) -> ExitCode {
        unsafe {
            while read_volatile(state.mboot.offset(0x20)) < id as u8 {
                thread::yield_now();
            }

            write_volatile(state.mboot.offset(0x30), id as u8);
        }

        let tmp = signal::SigSet::empty();
        let kvm_sigset = tmp.as_ref();

        let mut sig_mask = kvm_signal_mask::default();
        sig_mask.len = 8;
        let sig_mask_data = kvm_signal_mask_data { info: sig_mask, sigset: *kvm_sigset };

        let _ = VirtualCPU::set_signal_mask_fd(fd, sig_mask_data);

        let sigaction = signal::SigAction::new(
            signal::SigHandler::Handler(empty_handler),
            signal::SaFlags::empty(),
            signal::SigSet::empty(),
        );
        unsafe { let _ = signal::sigaction(signal::Signal::SIGUSR2, &sigaction); }

        let mut newset = signal::SigSet::empty();
        newset.add(signal::Signal::SIGUSR2);
        let oldset = newset.thread_swap_mask(signal::SigmaskHow::SIG_BLOCK).unwrap();

        while state.control.running.load(Ordering::Relaxed) {
            match VirtualCPU::single_run(fd, id, &state, &net_if) {
                Ok(proto::Return::Interrupt) => {
                    debug!("Interrupted {}", id);
                    if state.control.interrupt.load(Ordering::Relaxed) {
                        state.control.barrier.wait();
                        state.control.barrier.wait();
                    }

                    let _ = oldset.thread_set_mask();
                    let _ = newset.thread_block();
                },
                Ok(proto::Return::Exit(code)) => {
                    state.control.running.store(false, Ordering::Relaxed);

                    return ExitCode::Cause(Ok(code));
                },
                Err(err) => {
                    state.control.running.store(false, Ordering::Relaxed);
                    
                    return ExitCode::Cause(Err(err));
                },
                _ => {}
            }
        }

        ExitCode::Innocent
    }

    pub fn run(&mut self, net_if: Arc<Option<NetworkInterface>>) -> (JoinHandle<ExitCode>, pthread::Pthread, ::chan::Receiver<()>) {
        debug!("Run CPU {}", self.id);

        let state = self.state.take().unwrap();
        let id = self.id;
        let fd = self.vcpu_fd;

        let (spthread, rpthread) = ::chan::sync(0);
        let (sdone, rdone) = ::chan::sync(1);

        let handle = thread::spawn(move || {
            let _ = spthread.send(pthread::pthread_self());
            let ret = VirtualCPU::run_vcpu(state, id, fd, net_if);
            sdone.send(());
            ret
        });

        (handle, rpthread.recv().unwrap(), rdone)
    }

    pub fn setup_cpuid(&self) -> Result<()> {
        let mut kvm_cpuid = self.kvm.get_supported_cpuid()?;

        for entry in kvm_cpuid.data[0 .. kvm_cpuid.header.nent as usize].iter_mut() {
            match entry.function {
                1 => {
                    entry.ecx |= 1u32 << 31; // propagate that we are running on a hypervisor
                    if self.extensions.cap_tsc_deadline {
                        entry.eax |= 1u32 << 24; // enable TSC deadline feature
                    }
                    entry.edx |= 1u32 << 5; // enable msr support
                },
                CPUID_FUNC_PERFMON => {
                    // disable it
                    entry.eax = 0x00;
                },
                _ => {}
            }
        }

        self.set_cpuid2(kvm_cpuid)?;

        Ok(())
    }
}

impl Drop for VirtualCPU {
    fn drop(&mut self) {
        let _ = ::nix::unistd::close(self.vcpu_fd);
    }
}

unsafe impl Sync for SharedState {}
unsafe impl Send for SharedState {}
