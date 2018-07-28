mod header;
pub use self::header::*;

use libc::sigset_t;

#[repr(C)]
pub struct kvm_cpuid2_data {
    pub header: kvm_cpuid2,
    pub data: [kvm_cpuid_entry2; 100]
}

#[repr(C)]
#[derive(Default)]
pub struct kvm_msr_data {
	pub info: kvm_msrs,
	pub entries: [kvm_msr_entry; 25]
}

#[repr(C)]
pub struct kvm_signal_mask_data {
	pub info: kvm_signal_mask,
	pub sigset: sigset_t
}
