//! By calling create_vm KVM returns a fd, this file wraps all relevant functions belonging to the
//! VM layer

use std::io::Cursor;
use std::ffi::CStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Barrier};
use std::thread::JoinHandle;
use std::ptr;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::os::unix::io::RawFd;
use std::path::Path;
use std::net::Ipv4Addr;
use std::mem;
use std::rc::Rc;

use libc;
use memmap::Mmap;
use elf;
use elf::types::{ELFCLASS64, OSABI, PT_LOAD, ET_EXEC, EM_X86_64};
use chan_signal::Signal;
use nix::sys::pthread;
use nix::sys::mman::{madvise, MmapAdvise};

use hermit::is_verbose;
use hermit::IsleParameterUhyve;
use hermit::utils;
use hermit::uhyve;
use hermit::error::*;
use super::kvm::*;
use super::vcpu::{ExitCode, VirtualCPU, vcpu_state};
use super::proto::PORT_UART;
use super::checkpoint::{CheckpointConfig, FileCheckpoint};
use super::migration::{MigrationServer, MigrationClient};
use super::network::NetworkInterface;
use super::gdt;
use super::paging::*;

pub const GUEST_SIZE: usize = 0x20000000;

pub const BOOT_GDT:  usize = 0x1000;
pub const BOOT_INFO: usize = 0x2000;
pub const BOOT_PML4: usize = 0x10000;
pub const BOOT_PDPTE:usize = 0x11000;
pub const BOOT_PDE:  usize = 0x12000;

/// Basic CPU control in CR0
pub const X86_CR0_PE: u64 = (1 << 0);
pub const X86_CR0_PG: u64 = (1 << 31);

/// Intel long mode page directory/table entries
pub const X86_CR4_PAE: u64 = (1u64 << 5);

/// Intel long mode page directory/table entries
pub const X86_PDPT_P:  u64 = (1 << 0);
pub const X86_PDPT_RW: u64 = (1 << 1);
pub const X86_PDPT_PS: u64 = (1 << 7);

/// EFER bits:
pub const _EFER_SCE:    u64 = 0;  /* SYSCALL/SYSRET */
pub const _EFER_LME:    u64 = 8;  /* Long mode enable */
pub const _EFER_LMA:    u64 = 10; /* Long mode active (read-only) */
pub const _EFER_NX:     u64 = 11; /* No execute enable */
pub const _EFER_SVME:   u64 = 12; /* Enable virtualization */
pub const _EFER_LMSLE:  u64 = 13; /* Long Mode Segment Limit Enable */
pub const _EFER_FFXSR:  u64 = 14; /* Enable Fast FXSAVE/FXRSTOR */

pub const EFER_SCE:     u64 = (1<<_EFER_SCE);
pub const EFER_LME:     u64 = (1<<_EFER_LME);
pub const EFER_LMA:     u64 = (1<<_EFER_LMA);
pub const EFER_NX:      u64 = (1<<_EFER_NX);
pub const EFER_SVME:    u64 = (1<<_EFER_SVME);
pub const EFER_LMSLE:   u64 = (1<<_EFER_LMSLE);
pub const EFER_FFXSR:   u64 = (1<<_EFER_FFXSR);

///
pub const KVM_32BIT_MAX_MEM_SIZE:   usize = 1 << 32;
pub const KVM_32BIT_GAP_SIZE:       usize = 768 << 20;
pub const KVM_32BIT_GAP_START:      usize = KVM_32BIT_MAX_MEM_SIZE - KVM_32BIT_GAP_SIZE;

#[derive(Default, Clone)]
pub struct KVMExtensions {
    pub cap_tsc_deadline: bool,
	pub cap_irqchip: bool,
	pub cap_adjust_clock_stable: bool,
	pub cap_irqfd: bool,
	pub cap_vapic: bool,
}

pub struct ControlData {
    pub running: AtomicBool,
    pub interrupt: AtomicBool,
    pub barrier: Barrier
}

pub struct VirtualMachine {
    kvm: Rc<uhyve::KVM>,
    vm_fd: RawFd,
    mem: utils::MemoryMapMut,
    elf_entry: Option<u64>,
    klog: Option<*const i8>,
    mboot: Option<*mut u8>,
    vcpus: Vec<VirtualCPU>,
    num_cpus: u32,
    control: Arc<ControlData>,
    thread_handles: Vec<JoinHandle<ExitCode>>,
    extensions: KVMExtensions,
    additional: IsleParameterUhyve,
    checkpoint_num: u32
}

impl VirtualMachine {
    pub fn new(kvm: Rc<uhyve::KVM>, vm_fd: RawFd, size: usize, num_cpus: u32, add: IsleParameterUhyve) -> Result<VirtualMachine> {
        debug!("New virtual machine with memory size {}", size);

        // create a new memory region to map the memory of our guest
        let mut mem = if size < KVM_32BIT_GAP_START {
            utils::MemoryMapMut::anon(size)?
        } else {
            let mut tmp = utils::MemoryMapMut::anon(size + KVM_32BIT_GAP_START)?;
            
            unsafe { libc::mprotect(tmp.as_mut_ptr().offset(KVM_32BIT_GAP_START as isize) as *mut libc::c_void, KVM_32BIT_GAP_START, libc::PROT_NONE); }
            tmp
        };

        if add.mergable {
            unsafe {
                if madvise(mem.as_mut_ptr() as *mut libc::c_void, mem.len(), MmapAdvise::MADV_MERGEABLE).is_ok() {
                    debug!("Uhyve uses KSN feature \"mergeable\" to reduce the memory footprint.");
                }
            }
        }

        if add.hugepage {
            unsafe {
                if madvise(mem.as_mut_ptr() as *mut libc::c_void, mem.len(), MmapAdvise::MADV_HUGEPAGE).is_ok() {
                    debug!("Uhyve uses huge pages to improve the performance.");
                }
            }
        }

        let control = ControlData {
            running: AtomicBool::new(false),
            interrupt: AtomicBool::new(false),
            barrier: Barrier::new(num_cpus as usize + 1)
        };
        
        Ok(VirtualMachine {
            kvm: kvm,
            vm_fd: vm_fd,
            mem: mem,
            elf_entry: None,
            klog: None,
            vcpus: Vec::new(),
            mboot: None,
            num_cpus: num_cpus,
            control: Arc::new(control),
            thread_handles: Vec::new(),
            extensions: KVMExtensions::default(),
            additional: add,
            checkpoint_num: 0
        })
    }

    /// Loads a kernel from path and initialite mem and elf_entry
    pub fn load_kernel(&mut self, path: &str) -> Result<()> {
        debug!("Load kernel from {}", path);

        // open the file in read only
        let kernel_file = File::open(path).map_err(|_| Error::InvalidFile(path.into()))?;
        let file = unsafe { Mmap::map(&kernel_file) }.map_err(|_| Error::InvalidFile(path.into()))? ;

        // parse the header with ELF module
        let file_elf = {
            let mut data = Cursor::new(file.as_ref());
            
            elf::File::open_stream(&mut data)
                .map_err(|_| Error::InvalidFile(path.into()))
        }?;

        if file_elf.ehdr.class != ELFCLASS64
            || file_elf.ehdr.osabi != OSABI(0x42)
            || file_elf.ehdr.elftype != ET_EXEC
            || file_elf.ehdr.machine != EM_X86_64 {
            return Err(Error::InvalidFile(path.into()));
        }

        self.elf_entry = Some(file_elf.ehdr.entry);

        let mem_addr = self.mem.as_ptr() as u64;
        let mem_size = self.mem.len();

        let vm_mem = self.mem.as_mut();
        let kernel_file = file.as_ref();

        let mut pstart: Option<isize> = None;

        for header in file_elf.phdrs {
            if header.progtype != PT_LOAD {
                continue;
            }

            let vm_start = header.paddr as usize;
            let vm_end   = vm_start + header.filesz as usize;

            let kernel_start = header.offset as usize;
            let kernel_end   = kernel_start + header.filesz as usize;

            debug!("Load segment with start addr {} and size {} to {}", header.paddr, header.filesz, header.offset);

            vm_mem[vm_start..vm_end].copy_from_slice(&kernel_file[kernel_start..kernel_end]);
            
            unsafe {
                libc::memset(vm_mem.as_mut_ptr().offset(vm_end as isize) as *mut libc::c_void, 0x00, (header.memsz - header.filesz) as usize);
            }

            unsafe {
                if pstart.is_none() {
                    pstart = Some(header.paddr as isize);

                    let ptr = vm_mem.as_mut_ptr().offset(vm_start as isize);

                    *(ptr.offset(0x08) as *mut u64) = header.paddr;   // physical start addr
                    *(ptr.offset(0x10) as *mut u64) = mem_size as u64;  // physical size limit
                    *(ptr.offset(0x18) as *mut u32) = utils::cpufreq()?; // CPU frequency
                    *(ptr.offset(0x24) as *mut u32) = 1;              // number of used CPUs
                    *(ptr.offset(0x30) as *mut u32) = 0;              // apicid (?)
                    *(ptr.offset(0x60) as *mut u32) = 1;              // NUMA nodes
                    *(ptr.offset(0x94) as *mut u32) = 1;              // announce uhyve
                    if is_verbose() {
                        *(ptr.offset(0x98) as *mut u64) = PORT_UART as u64;              // announce uhyve
                    }

                    if let Some(ip) = self.additional.ip {
                        let data = ip.octets();
                        *(ptr.offset(0xB0) as *mut u8) = data[0];
                        *(ptr.offset(0xB1) as *mut u8) = data[1];
                        *(ptr.offset(0xB2) as *mut u8) = data[2];
                        *(ptr.offset(0xB3) as *mut u8) = data[3];
                    }

                    if let Some(gateway) = self.additional.gateway {
                        let data = gateway.octets();
                        *(ptr.offset(0xB4) as *mut u8) = data[0];
                        *(ptr.offset(0xB5) as *mut u8) = data[1];
                        *(ptr.offset(0xB6) as *mut u8) = data[2];
                        *(ptr.offset(0xB7) as *mut u8) = data[3];
                    }

                    if let Some(mask) = self.additional.mask {
                        let data = mask.octets();
                        *(ptr.offset(0xB8) as *mut u8) = data[0];
                        *(ptr.offset(0xB9) as *mut u8) = data[1];
                        *(ptr.offset(0xBA) as *mut u8) = data[2];
                        *(ptr.offset(0xBB) as *mut u8) = data[3];
                    }

                    *(ptr.offset(0xBC) as *mut u64) = mem_addr;

                    self.klog = Some(ptr.offset(0x5000) as *const i8);
                    self.mboot = Some(ptr);
                }

                let start = pstart.unwrap();
                *(vm_mem.as_mut_ptr().offset(start + 0x38) as *mut u64) = header.paddr + header.memsz - start as u64; // total kernel size
            }
        }

        debug!("Kernel loaded");

        Ok(())
    }

    pub fn load_checkpoint(&mut self, chk: &CheckpointConfig) -> Result<()> {
        unsafe {
            self.klog = Some(self.mem.as_ptr().offset(chk.get_elf_entry() as isize + 0x5000) as *const i8);
            self.mboot = Some(self.mem.as_mut_ptr().offset(chk.get_elf_entry() as isize) as *mut u8);
        }

        self.elf_entry = Some(chk.get_elf_entry());

        let chk_num = chk.get_checkpoint_number();
        let start = if chk.get_full() { chk_num } else { 0 };

        for i in start .. chk_num+1 {
            let file_name = FileCheckpoint::get_mem_file_path(i);
            let mut file = File::open(&file_name).map_err(|_| Error::InvalidFile(file_name.clone()))?;
            
            let mut clock = kvm_clock_data::default();
            file.read_exact(unsafe { utils::any_as_u8_mut_slice(&mut clock) }).map_err(|_| Error::InvalidFile(file_name.clone()))?;

            if self.extensions.cap_adjust_clock_stable && i == chk_num {
                let mut data = kvm_clock_data::default();
                data.clock = clock.clock;

                let _ = self.set_clock(data);
            }
            
            let mut location: PageTableEntry = unsafe { ::std::mem::uninitialized() };
            while let Ok(()) = VirtualMachine::read_page_table_entry(&mut file, &mut location) {
                let dest_addr = unsafe { self.mem.as_mut_ptr().offset(location.address() as isize) };
                let len = if location.is_hugepage() { LargePageSize::SIZE } else { BasePageSize::SIZE };
                let dest = unsafe { ::std::slice::from_raw_parts_mut(dest_addr, len) };
                file.read_exact(dest).map_err(|_| Error::InvalidFile(file_name.clone()))?;
            }
        }

        self.checkpoint_num = chk.get_checkpoint_number() + 1;

        debug!("Loaded checkpoint {}", chk_num);

        Ok(())
    }

    fn read_page_table_entry(file: &mut File, loc: &mut PageTableEntry) -> ::std::io::Result<()> {
        let loc_data = unsafe { utils::any_as_u8_mut_slice(loc) };
        file.read_exact(loc_data)
    }

    pub fn load_migration(&mut self, mig: &mut MigrationServer) -> Result<()> {
        let entry = mig.get_metadata().get_elf_entry();

        unsafe {
            self.klog = Some(self.mem.as_ptr().offset(entry as isize + 0x5000) as *const i8);
            self.mboot = Some(self.mem.as_mut_ptr().offset(entry as isize) as *mut u8);
        }

        self.elf_entry = Some(entry);

        mig.recv_data(self.mem.as_mut())?;
        debug!("Guest memory received: {}", self.mem.len());

        mig.recv_cpu_states()?;

        if self.extensions.cap_adjust_clock_stable {
            let mut clock = kvm_clock_data::default();
            mig.recv_data(unsafe { utils::any_as_u8_mut_slice(&mut clock) })?;
            debug!("Received clock: {}", ::std::mem::size_of::<kvm_clock_data>());

            let mut data = kvm_clock_data::default();
            data.clock = clock.clock;

            let _ = self.set_clock(data);
        }

        debug!("Loaded migration");

        Ok(())
    }

    /// Initialize the virtual machine
    pub fn init(&mut self) -> Result<()> {
        let mut identity_base: u64 = 0xfffbc000;
        
        if let Ok(true) = self.check_extension(KVM_CAP_SYNC_MMU) {
            identity_base = 0xfeffc000;

            self.set_identity_map_addr(identity_base)?;
        }
        
        self.set_tss_addr(identity_base+0x1000)?;

        let mut kvm_region = kvm_userspace_memory_region {
            slot: 0,
            guest_phys_addr: 0,
            flags: 0,
            memory_size: self.mem_size() as u64,
            userspace_addr: self.mem.as_ptr() as u64
        };

        if self.mem_size() <= KVM_32BIT_GAP_START {
            self.set_user_memory_region(kvm_region)?;
        } else {
            kvm_region.memory_size = KVM_32BIT_GAP_START as u64;
            self.set_user_memory_region(kvm_region)?;

            kvm_region.slot = 1;
            kvm_region.guest_phys_addr = (KVM_32BIT_GAP_START + KVM_32BIT_GAP_SIZE) as u64;
            kvm_region.userspace_addr = self.mem.as_ptr() as u64 + (KVM_32BIT_GAP_START + KVM_32BIT_GAP_SIZE) as u64;
            kvm_region.memory_size = (self.mem_size() - KVM_32BIT_GAP_SIZE - KVM_32BIT_GAP_START) as u64;
            self.set_user_memory_region(kvm_region)?;
        }

        self.create_irqchip()?;

        // KVM_CAP_X2APIC_API
        let mut cap = kvm_enable_cap::default();
        cap.cap = KVM_CAP_X2APIC_API;
        cap.args[0] = (KVM_X2APIC_API_USE_32BIT_IDS|KVM_X2APIC_API_DISABLE_BROADCAST_QUIRK) as u64;
        self.enable_cap(cap)?;

        let mut chip = kvm_irqchip::default();
        chip.chip_id = KVM_IRQCHIP_IOAPIC;
        self.get_irqchip(&mut chip)?;
        for i in 0 .. KVM_IOAPIC_NUM_PINS as usize {
            unsafe {
            chip.chip.ioapic.redirtbl[i].fields.vector = 0x20+i as u8;
            chip.chip.ioapic.redirtbl[i].fields._bitfield_1 = kvm_ioapic_state__bindgen_ty_1__bindgen_ty_1::new_bitfield_1(
                0, // delivery_mode
                0, // dest_mode
                0, // delivery_status
                0, // polarity
                0, // remote_irr
                0, // trig_mode
                if i != 2 { 0 } else { 1 }, // mask
                0, // reserve
            );
            chip.chip.ioapic.redirtbl[i].fields.dest_id = 0;
            }
        }
        self.set_irqchip(chip)?;

        self.extensions.cap_tsc_deadline = self.check_extension(KVM_CAP_TSC_DEADLINE_TIMER)?;
        self.extensions.cap_irqchip = self.check_extension(KVM_CAP_IRQCHIP)?;
        self.extensions.cap_adjust_clock_stable = self.check_extension_int(KVM_CAP_ADJUST_CLOCK)? == KVM_CLOCK_TSC_STABLE as i32;
        self.extensions.cap_irqfd = self.check_extension(KVM_CAP_IRQFD)?;
        self.extensions.cap_vapic = self.check_extension(KVM_CAP_VAPIC)?;

        if !self.extensions.cap_irqfd {
            return Err(Error::CAPIRQFD)
        }

        Ok(())
    }

    pub fn create_cpus(&mut self) -> Result<()> {
        for i in 0..self.num_cpus {
            self.create_vcpu(i as u32)?;
        }

        Ok(())
    }

    pub fn init_sregs(&mut self, mut sregs: &mut kvm_sregs) -> Result<()> {
        debug!("Setup GDT");
        self.setup_system_gdt(&mut sregs, BOOT_GDT as u64)?;
        debug!("Setup the page tables");
        self.setup_system_page_tables(&mut sregs)?;
        debug!("Set the system to 64bit");
        self.setup_system_64bit(&mut sregs)?;

        Ok(())
    }

    pub fn setup_system_gdt(&mut self, sregs: &mut kvm_sregs, offset: u64) -> Result<()> {
        let (mut data_seg, mut code_seg) = (kvm_segment::default(), kvm_segment::default());               

        // create the GDT entries
        let gdt_null = gdt::Entry::new(0, 0, 0);
        let gdt_code = gdt::Entry::new(0xA09B, 0, 0xFFFFF);
        let gdt_data = gdt::Entry::new(0xC093, 0, 0xFFFFF);

        // apply the new GDTs to our guest memory
        unsafe {
            let mem = self.mem.as_mut_ptr();
            let ptr = mem.offset(offset as isize) as *mut u64;
            
            *(ptr.offset(gdt::BOOT_NULL)) = gdt_null.as_u64();
            *(ptr.offset(gdt::BOOT_CODE)) = gdt_code.as_u64();
            *(ptr.offset(gdt::BOOT_DATA)) = gdt_data.as_u64();
        }

        gdt_code.apply_to_kvm(gdt::BOOT_CODE, &mut code_seg);
        gdt_data.apply_to_kvm(gdt::BOOT_DATA, &mut data_seg);

        sregs.gdt.base = offset;
        sregs.gdt.limit = ((mem::size_of::<u64>() * gdt::BOOT_MAX) - 1) as u16;
        sregs.cs = code_seg;
        sregs.ds = data_seg;
        sregs.es = data_seg;
        sregs.fs = data_seg;
        sregs.gs = data_seg;
        sregs.ss = data_seg;

        Ok(())
    }

    pub fn setup_system_page_tables(&mut self, sregs: &mut kvm_sregs) -> Result<()> {
        unsafe {
            let mem = self.mem.as_mut_ptr();
            let pml4 = &mut *(mem.offset(BOOT_PML4 as isize) as *mut PageTable);
            let pdpte = &mut *(mem.offset(BOOT_PDPTE as isize) as *mut PageTable);
            let pde = &mut *(mem.offset(BOOT_PDE as isize) as *mut PageTable);

            utils::memzero(pml4);
            utils::memzero(pdpte);
            utils::memzero(pde);
            
            pml4.entries[0].set(BOOT_PDPTE, PageTableEntryFlags::PRESENT | PageTableEntryFlags::WRITABLE);
            pdpte.entries[0].set(BOOT_PDE, PageTableEntryFlags::PRESENT | PageTableEntryFlags::WRITABLE);
           
            for i in 0 .. GUEST_SIZE / LargePageSize::SIZE {
                pdpte.entries[i].set(i * LargePageSize::SIZE,
                    PageTableEntryFlags::PRESENT | PageTableEntryFlags::WRITABLE | PageTableEntryFlags::HUGE_PAGE);
            }
        }

        sregs.cr3 = BOOT_PML4 as u64;
        sregs.cr4 |= X86_CR4_PAE;
        sregs.cr0 |= X86_CR0_PG;

        Ok(())
    }

    pub fn setup_system_64bit(&self, sregs: &mut kvm_sregs) -> Result<()> {
        sregs.cr0 |= X86_CR0_PE;
        sregs.cr4 |= X86_CR4_PAE;
        sregs.efer |= EFER_LME|EFER_LMA;

        Ok(())
    }

    pub fn init_cpus(&mut self) -> Result<()> {
        let entry = self.elf_entry.ok_or(Error::KernelNotLoaded)?;

        let mut sregs = self.vcpus[0].get_sregs()?;
        self.init_sregs(&mut sregs)?;

        for cpu in &self.vcpus {
            cpu.init(entry)?;
            cpu.set_sregs(sregs)?;
        }

        Ok(())
    }

    pub fn restore_cpus(&mut self, cpu_states: &mut Vec<vcpu_state>) -> Result<()> {
        if cpu_states.len() < self.vcpus.len() {
            return Err(Error::VCPUStatesNotInitialized)
        }

        for cpu in &self.vcpus {
            cpu.restore_cpu_state(&mut cpu_states[cpu.get_id() as usize])?;
        }

        Ok(())
    }

    pub fn set_user_memory_region(&self, mut region: kvm_userspace_memory_region) -> Result<()> {
        unsafe {
            uhyve::ioctl::set_user_memory_region(self.vm_fd, (&mut region) as *mut kvm_userspace_memory_region)
                .map_err(|_| Error::IOCTL(NameIOCTL::SetUserMemoryRegion)).map(|_| ())
        }
    }

    pub fn create_irqchip(&self) -> Result<()> {
        unsafe {
            uhyve::ioctl::create_irqchip(self.vm_fd, ptr::null_mut())
                .map_err(|_| Error::IOCTL(NameIOCTL::CreateIRQChip)).map(|_| ())
        }
    }

    pub fn check_extension(&self, extension: u32) -> Result<bool> {
        self.check_extension_int(extension).map(|x| x > 0)
    }

    pub fn check_extension_int(&self, extension: u32) -> Result<i32> {
        unsafe {
            uhyve::ioctl::check_extension(self.vm_fd, extension as *mut u8)
                .map_err(|_| Error::IOCTL(NameIOCTL::CheckExtension))
        }
    }

    pub fn set_identity_map_addr(&self, identity_base: u64) -> Result<()> {
        unsafe {
            uhyve::ioctl::set_identity_map_addr(self.vm_fd, (&identity_base) as *const u64)
                .map_err(|_| Error::IOCTL(NameIOCTL::SetTssIdentity)).map(|_| ())
        }
    }

    pub fn set_tss_addr(&self, identity_base: u64) -> Result<()> {
        unsafe {
            uhyve::ioctl::set_tss_addr(self.vm_fd, identity_base as *mut u8)
                .map_err(|_| Error::IOCTL(NameIOCTL::SetTssAddr)).map(|_| ())
        }
    }

    pub fn enable_cap(&self, mut region: kvm_enable_cap) -> Result<()> {
        unsafe {
            uhyve::ioctl::enable_cap(self.vm_fd, (&mut region) as *mut kvm_enable_cap)
                .map_err(|_| Error::IOCTL(NameIOCTL::EnableCap)).map(|_| ())
        }
    }

    fn get_irqchip(&self, chip: &mut kvm_irqchip) -> Result<()> {
        unsafe {
            uhyve::ioctl::get_irqchip(self.vm_fd, chip as *mut kvm_irqchip)
                .map_err(|_| Error::IOCTL(NameIOCTL::GetIRQChip))?;
        }

        Ok(())
    }

    pub fn set_irqchip(&self, mut chip: kvm_irqchip) -> Result<()> {
        unsafe {
            uhyve::ioctl::set_irqchip(self.vm_fd, (&mut chip) as *mut kvm_irqchip)
                .map_err(|_| Error::IOCTL(NameIOCTL::SetIRQChip))?;
        }

        Ok(())
    }

    fn get_clock(&self, clock: &mut kvm_clock_data) -> Result<()> {
        unsafe {
            uhyve::ioctl::get_clock(self.vm_fd, clock as *mut kvm_clock_data)
                .map_err(|_| Error::IOCTL(NameIOCTL::GetClock))?;
        }

        Ok(())
    }

    pub fn set_clock(&self, mut clock: kvm_clock_data) -> Result<()> {
        unsafe {
            uhyve::ioctl::set_clock(self.vm_fd, (&mut clock) as *mut kvm_clock_data)
                .map_err(|_| Error::IOCTL(NameIOCTL::SetClock))?;
        }

        Ok(())
    }

    pub fn create_vcpu(&mut self, id: u32) -> Result<()> {
        let vcpu_fd = unsafe { uhyve::ioctl::create_vcpu(self.vm_fd, id as i32)
            .map_err(|_| Error::IOCTL(NameIOCTL::CreateVcpu))? };
        let mmap_size = self.kvm.get_mmap_size()?;
        let cpu = VirtualCPU::new(self.kvm.clone(), vcpu_fd, id, mmap_size,
            &mut self.mem, self.mboot.unwrap(), self.control.clone(), self.extensions.clone())?;
        self.vcpus.insert(id as usize, cpu);

        Ok(())
    }

    pub fn output(&self) -> String {
        match self.klog {
            Some(paddr) => {
                let c_str = unsafe { CStr::from_ptr(paddr) };
                c_str.to_str().unwrap_or("").into()
            },
            None => "".into()
        }

    }

    fn scan_page_tables(&mut self, mut file: File) -> Result<()> {
        let flag = if !self.additional.full_checkpoint && self.checkpoint_num > 0 {
            PageTableEntryFlags::DIRTY
        } else {
            PageTableEntryFlags::ACCESSED
        };

        let mem_ptr = self.mem.as_mut_ptr();
        let entry = self.elf_entry.ok_or(Error::KernelNotLoaded)? as isize;

        let pml4 = unsafe { &mut *(mem_ptr.offset(entry + BasePageSize::SIZE as isize) as *mut PageTable) };
        for pml4e in pml4.entries.iter() {
            if !pml4e.is_present() {
                continue;
            }

            let pdpt = unsafe { &mut *(mem_ptr.offset(pml4e.address() as isize) as *mut PageTable) };
            for pdpte in pdpt.entries.iter() {
                if !pdpte.is_present() {
                    continue;
                }

                let pgd = unsafe { &mut *(mem_ptr.offset(pdpte.address() as isize) as *mut PageTable) };
                for pgde in pgd.entries.iter_mut() {
                    if !pgde.is_present() {
                        continue;
                    }

                    if !pgde.is_hugepage() {
                        let pgt = unsafe { &mut *(mem_ptr.offset(pgde.address() as isize) as *mut PageTable) };
                        for pgte in pgt.entries.iter_mut() {
                            if pgte.check_flags(PageTableEntryFlags::PRESENT | flag) {
                                if !self.additional.full_checkpoint {
                                    let flags = pgte.flags();
                                    pgte.set_flags(flags & !(PageTableEntryFlags::DIRTY|PageTableEntryFlags::ACCESSED).bits());
                                }

                                let mut pgte_tmp = pgte.clone();
                                let flags = pgte_tmp.flags();
                                pgte_tmp.set_flags(flags & !PageTableEntryFlags::HUGE_PAGE.bits());

                                unsafe {
                                    let mut data = utils::any_as_u8_slice(&pgte_tmp);
                                    file.write_all(data).map_err(|_| Error::WriteCheckpoint)?;

                                    let data_ptr = mem_ptr.offset(pgte.address() as isize);
                                    let mut data = ::std::slice::from_raw_parts(data_ptr, BasePageSize::SIZE);
                                    file.write_all(data).map_err(|_| Error::WriteCheckpoint)?;
                                }
                            }
                        }
                    } else if pgde.check_flags(flag) {
                        if !self.additional.full_checkpoint {
                            let flags = pgde.flags();
                            pgde.set_flags(flags & !(PageTableEntryFlags::DIRTY|PageTableEntryFlags::ACCESSED).bits());
                        }

                        unsafe {
                            let mut data = utils::any_as_u8_slice(pgde);
                            file.write_all(data).map_err(|_| Error::WriteCheckpoint)?;

                            let data_ptr = mem_ptr.offset(pgde.address() as isize);
                            let mut data = ::std::slice::from_raw_parts(data_ptr, LargePageSize::SIZE);
                            file.write_all(data).map_err(|_| Error::WriteCheckpoint)?;
                        }
                    }
                }
            }
        }

        Ok(())
    }

    fn get_checkpoint_config(&self) -> Result<CheckpointConfig> {
        Ok(CheckpointConfig {
            num_cpus: self.num_cpus,
            mem_size: self.mem.len() as isize,
            checkpoint_number: self.checkpoint_num,
            elf_entry: self.elf_entry.ok_or(Error::KernelNotLoaded)?,
            full: self.additional.full_checkpoint
        })
    }

    fn create_checkpoint(&mut self) -> Result<()> {
        if !Path::new("checkpoint").exists() {
            fs::create_dir("checkpoint").map_err(|_| Error::WriteCheckpoint)?;
        }

        let mut chk = FileCheckpoint::new(self.get_checkpoint_config()?);

        for cpu in &self.vcpus {
            chk.get_cpu_states().push(cpu.save_cpu_state()?);
        }

        let file_name = FileCheckpoint::get_mem_file_path(self.checkpoint_num);
        let mut file = File::create(&file_name).map_err(|_| Error::WriteCheckpoint)?;

        let mut clock = kvm_clock_data::default();
        self.get_clock(&mut clock)?;

        file.write_all(unsafe { utils::any_as_u8_slice(&clock) })
            .map_err(|_| Error::WriteCheckpoint)?;

        self.scan_page_tables(file)?;

        chk.save()?;

        self.checkpoint_num += 1;

        Ok(())
    }

    fn handle_checkpoint(&mut self, threads: &Vec<pthread::Pthread>) {
        self.control.interrupt.store(true, Ordering::Relaxed);
        for thr in threads {
            unsafe { libc::pthread_kill(*thr, libc::SIGUSR2); }
        }

        self.control.barrier.wait();

        match self.create_checkpoint() {
            Ok(()) => debug!("Successfully wrote checkpoint"),
            Err(e) => eprintln!("Failed to write checkpoint: {}", e)
        };

        self.control.interrupt.store(false, Ordering::Relaxed);
        self.control.barrier.wait();
    }

    fn run_migration(&self, mig_dest: Ipv4Addr) -> Result<()> {
        let mut client = MigrationClient::connect(mig_dest)?;

        let mut meta = self.get_checkpoint_config()?;
        meta.checkpoint_number = 0;

        client.send_data(unsafe { utils::any_as_u8_slice(&meta) })?;

        debug!("Sent meta: {}", ::std::mem::size_of::<CheckpointConfig>());

        client.send_data(self.mem.as_ref())?;

        debug!("Sent mem: {}", self.mem.len());

        for cpu in &self.vcpus {
            let cpu_state = cpu.save_cpu_state()?;
            client.send_data(unsafe { utils::any_as_u8_slice(&cpu_state) })?;
            debug!("Sent vcpu: {}", ::std::mem::size_of::<vcpu_state>());
        }

        if self.extensions.cap_adjust_clock_stable {
            let mut clock = kvm_clock_data::default();
            self.get_clock(&mut clock)?;

            client.send_data(unsafe { utils::any_as_u8_slice(&clock) })?;
            debug!("Sent clock: {}", ::std::mem::size_of::<kvm_clock_data>());
        }

        Ok(())
    }

    fn handle_migration(&self, threads: &Vec<pthread::Pthread>) -> bool {
        let mig_dest = match self.additional.migration_support {
            Some(dest) => dest,
            None => return true
        };

        self.control.interrupt.store(true, Ordering::Relaxed);
        for thr in threads {
            unsafe { libc::pthread_kill(*thr, libc::SIGUSR2); }
        }

        self.control.barrier.wait();

        let ret = match self.run_migration(mig_dest) {
            Ok(()) => {
                println!("Successfully sent migration data");
                false
            },
            Err(e) => {
                eprintln!("Failed to send migration data: {}", e);
                true
            }
        };

        self.control.interrupt.store(false, Ordering::Relaxed);
        self.control.barrier.wait();

        ret
    }

    fn handle_signal(&self, sig: &Signal, threads: &Vec<pthread::Pthread>) -> bool {
        match sig {
            Signal::INT | Signal::TERM => false,
            Signal::USR1 => self.handle_migration(threads),
            _ => true
        }
    }

    pub fn run(&mut self) -> Result<()> {
        unsafe { *(self.mboot.unwrap().offset(0x24) as *mut u32) = self.num_cpus; }
        self.control.running.store(true, Ordering::Relaxed);

        let signal = ::chan_signal::notify(&[Signal::INT, Signal::TERM, Signal::USR1]);

        let net_if = Arc::new(match &self.additional.netif {
            Some(netif) => Some(NetworkInterface::new(self.vm_fd, netif, &self.additional.mac_addr)?),
            None => None
        });

        let mut pthreads = Vec::new();

        let rdone = {
            let (handle, ptid, recv) = self.vcpus[0].run(net_if.clone());
            self.thread_handles.push(handle);
            pthreads.push(ptid);
            recv
        };

        for vcpu in &mut self.vcpus[1..] {
            let (handle, ptid, _) = vcpu.run(net_if.clone());
            self.thread_handles.push(handle);
            pthreads.push(ptid);
        }

        let cp_tick = ::chan::tick_ms(1000);
        let threads = pthreads.clone();

        let mut tick = 0;

        loop {
            chan_select! {
                signal.recv() -> sig => if !self.handle_signal(&sig.unwrap(), &threads) { break },
                cp_tick.recv() => {
                    tick += 1;
                    if self.additional.checkpoint > 0 && tick % self.additional.checkpoint == 0 {
                        self.handle_checkpoint(&threads);
                    }
                },
                rdone.recv() => break
            }
        }

        self.control.running.store(false, Ordering::Relaxed);

        // interrupt all threads
        for thr in pthreads {
            unsafe { libc::pthread_kill(thr, libc::SIGUSR2); }
        }

        self.stop()?;
        Ok(())
    }

    pub fn stop(&mut self) -> Result<()> {
        self.control.running.store(false, Ordering::Relaxed);

        let mut result = Ok(());
        while let Some(handle) = self.thread_handles.pop() {
            if let Ok(ret) = handle.join() {
                match ret {
                    ExitCode::Innocent => continue,
                    ExitCode::Cause(cause) => match cause {
                        Ok(code) => debug!("Thread has exited with code {}", code),
                        Err(e) => {
                            eprintln!("Thread error: {}", e);
                            result = Err(Error::ThreadError)
                        }
                    }
                }
            }
        }

        result
    }

    pub fn is_running(&mut self) -> Result<bool> {
        Ok(self.control.running.load(Ordering::Relaxed))
    }

    pub fn mem_size(&self) -> usize {
        self.mem.len()
    }
}

impl Drop for VirtualMachine {
    fn drop(&mut self) {
        debug!("Drop the Virtual Machine");
        //debug!("-------- Output --------");
        //debug!("{}", self.output());
        let _ = ::nix::unistd::close(self.vm_fd);
    }
}
