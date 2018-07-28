use std::ffi::{CString, CStr};
use std::env;
use std::fs;
use std::path::Path;
use std::sync::Arc;

use libc::{write, read, lseek, open, close, strcpy, c_void, c_char};

use hermit::error::*;
use hermit::uhyve::kvm::*;
use hermit::is_verbose;
use super::network::NetworkInterface;

const PORT_WRITE:   u16 = 0x400;
const PORT_OPEN:    u16 = 0x440;
const PORT_CLOSE:   u16 = 0x480;
const PORT_READ:    u16 = 0x500;
const PORT_EXIT:    u16 = 0x540;
const PORT_LSEEK:   u16 = 0x580;

const PORT_NETINFO:     u16 = 0x600;
const PORT_NETWRITE:    u16 = 0x640;
const PORT_NETREAD:     u16 = 0x680;
const PORT_NETSTAT:     u16 = 0x700;

const PORT_CMDSIZE: u16 = 0x740;
const PORT_CMDVAL:  u16 = 0x780;

pub const PORT_UART:    u16 = 0x800;

#[repr(C, packed)]
pub struct Exit {
    arg: i32
}

#[repr(C, packed)]
pub struct Write {
    fd: i32,
    buf: isize,
    length: usize
}

#[repr(C, packed)]
pub struct Open {
    name: isize,
    flags: i32,
    mode: i32,
    ret: i32
}

#[repr(C, packed)]
pub struct Close {
    fd: i32,
    ret: i32
}

#[repr(C, packed)]
pub struct Read {
    fd: i32,
    buf: isize,
    len: usize,
    ret: isize
}

#[repr(C, packed)]
pub struct LSeek {
    fd: i32,
    offset: i64,
    whence: i32,
}

#[repr(C, packed)]
pub struct NetInfo {
	mac_str: [u8; 18]
}

#[repr(C, packed)]
pub struct NetWrite {
	data: isize,
	len: usize,
	ret: i32
}

#[repr(C, packed)]
pub struct NetRead {
	data: isize,
	len: usize,
	ret: i32
}

#[repr(C, packed)]
pub struct NetStat {
    status: i32
}

#[repr(C, packed)]
pub struct CmdSize {
	argc: i32,
	argsz: [i32; 128],
	envc: i32,
	envsz: [i32; 128],
}

#[repr(C, packed)]
pub struct CmdVal {
	argv: isize,
	envp: isize
}

#[derive(Debug)]
pub enum Syscall {
    UART(u8),
    Write(*mut Write),
    Open(*mut Open),
    Close(*mut Close),
    Read(*mut Read),
    LSeek(*mut LSeek),
    Exit(*mut Exit),
    NetInfo(*mut NetInfo),
    NetWrite(*mut NetWrite),
    NetRead(*mut NetRead),
    NetStat(*mut NetStat),
    CmdSize(*mut CmdSize),
    CmdVal(*mut CmdVal),
    Other(*const kvm_run)
}

#[derive(Debug)]
pub enum Return {
    Continue,
    Interrupt,
    Exit(i32)
}

impl Syscall {
    pub fn from_mem(mem: *const u8, guest_mem: *mut u8) -> Result<Syscall> {
        unsafe {
            let ref run = *(mem as *const kvm_run);

            // debug!("Exit reason {}", run.exit_reason);

            // TODO: KVM_EXIT_MMIO
            if run.exit_reason != KVM_EXIT_IO {
                return Ok(Syscall::Other(mem as *const kvm_run));
            }

            let offset = *(mem.offset(run.__bindgen_anon_1.io.data_offset as isize) as *const isize);
            let ptr = guest_mem.offset(offset);
            Ok(match run.__bindgen_anon_1.io.port {
                PORT_UART       => { Syscall::UART(offset as u8) },
                PORT_WRITE      => { Syscall::Write(ptr as *mut Write) },
                PORT_READ       => { Syscall::Read (ptr as *mut Read)  },
                PORT_CLOSE      => { Syscall::Close(ptr as *mut Close) },
                PORT_OPEN       => { Syscall::Open (ptr as *mut Open ) },
                PORT_LSEEK      => { Syscall::LSeek(ptr as *mut LSeek) },
                PORT_EXIT       => { Syscall::Exit (ptr as *mut Exit) },
                PORT_NETINFO    => { Syscall::NetInfo(ptr as *mut NetInfo) },
                PORT_NETWRITE   => { Syscall::NetWrite(ptr as *mut NetWrite) },
                PORT_NETREAD    => { Syscall::NetRead(ptr as *mut NetRead) },
                PORT_NETSTAT    => { Syscall::NetStat(ptr as *mut NetStat) },
                PORT_CMDSIZE    => { Syscall::CmdSize(ptr as *mut CmdSize) },
                PORT_CMDVAL     => { Syscall::CmdVal(ptr as *mut CmdVal) },
                _ => {
                    let err = format!("KVM: unhandled KVM_EXIT_IO at port {:#x}, direction {}",
                        run.__bindgen_anon_1.io.port, run.__bindgen_anon_1.io.direction);
                    return Err(Error::Protocol(err));
                }
            })
        }
    }

    pub unsafe fn run(&self, guest_mem: *mut u8, net_if: &Arc<Option<NetworkInterface>>) -> Result<Return> {
        match *self {
            Syscall::UART(obj) => {
                if is_verbose() {
                    use std::io::{self, Write};
                    let buf = [obj];
                    io::stderr().write(&buf).ok();
                }
            },
            Syscall::Write(obj) => {
                (*obj).length = write((*obj).fd, guest_mem.offset((*obj).buf) as *const c_void, (*obj).length) as usize;
            },
            Syscall::Read(obj) => {
                (*obj).ret = read((*obj).fd, guest_mem.offset((*obj).buf) as *mut c_void, (*obj).len);
            },
            Syscall::Exit(obj) => {
                return Ok(Return::Exit(*(guest_mem.offset((*obj).arg as isize)) as i32));
            },
            Syscall::Open(obj) => {
                let name_ptr = guest_mem.offset((*obj).name) as *const i8;
                let name = CStr::from_ptr(name_ptr).to_str().unwrap();
                let kvm = fs::canonicalize(name).map(|path| path == Path::new("/dev/kvm")).unwrap_or(false);
                (*obj).ret = if kvm { -1 } else { open(name_ptr, (*obj).flags, (*obj).mode) };
            },
            Syscall::Close(obj) => {
                (*obj).ret = match (*obj).fd {
                    n if n > 2 => close((*obj).fd),
                    _ => 0
                }
            },
            Syscall::LSeek(obj) => {
                (*obj).offset = lseek((*obj).fd, (*obj).offset, (*obj).whence);
            },
            Syscall::NetInfo(obj) => {
                println!("info");
                if net_if.is_some() {
                    match Option::as_ref(&net_if) {
                        Some(net) => {
                            (*obj).mac_str.copy_from_slice(net.get_mac());
                        },
                        None => {}
                    }
                }
            },
            Syscall::NetWrite(obj) => {
                let netfd = match Option::as_ref(&net_if) {
                    Some(net) => net.get_netfd(),
                    None => -1
                };
                let ret = write(netfd, guest_mem.offset((*obj).data) as *const c_void, (*obj).len);
                if ret >= 0 {
                    (*obj).ret = 0;
                    (*obj).len = ret as usize;
                } else {
                    (*obj).ret = -1;
                }
            },
            Syscall::NetRead(obj) => {
                let netfd = match Option::as_ref(&net_if) {
                    Some(net) => net.get_netfd(),
                    None => -1
                };
                let ret = read(netfd, guest_mem.offset((*obj).data) as *mut c_void, (*obj).len);
                if ret >= 0 {
                    (*obj).ret = 0;
                    (*obj).len = ret as usize;
                } else {
                    (*obj).ret = -1;
                    match Option::as_ref(&net_if) {
                        Some(net) => net.notify(),
                        None => {}
                    }
                }
            },
            Syscall::NetStat(obj) => {
                (*obj).status = net_if.is_some() as i32;
            },
            Syscall::CmdSize(obj) => {
                (*obj).argc = env::args().count() as i32 - 1;
                let mut count = 0;
                for key in env::args().skip(1) {
                    (*obj).argsz[count] = key.len() as i32 + 1;
                    count += 1;
                }

                (*obj).envc = env::vars().count() as i32;
                count = 0;
                for (val,key) in env::vars() {
                    let tmp = format!("{}={}", val, key);
                    (*obj).envsz[count] = tmp.len() as i32;
                    count += 1;
                }
            },
            Syscall::CmdVal(obj) => {
                let argv_ptr = guest_mem.offset((*obj).argv) as *const isize;
                let envp_ptr = guest_mem.offset((*obj).envp) as *const isize;

                let mut count = 0;
                for key in env::args().skip(1) {
                    let c_str = CString::new(key).unwrap();
                    strcpy(guest_mem.offset(*argv_ptr.offset(count)) as *mut c_char, c_str.as_ptr());
                    count += 1;
                }
                
                count = 0;
                for (val,key) in env::vars() {
                    let c_str = CString::new(format!("{}={}", val, key)).unwrap();
                    strcpy(guest_mem.offset(*envp_ptr.offset(count)) as *mut c_char, c_str.as_ptr());
                    count += 1;
                }
            },
            Syscall::Other(id) => {
                let err = match (*id).exit_reason {
                    KVM_EXIT_HLT => format!("Guest has halted the CPU, this is considered as a normal exit."),
                    KVM_EXIT_MMIO => format!("KVM: unhandled KVM_EXIT_MMIO at {:#x}", (*id).__bindgen_anon_1.mmio.phys_addr ),
                    KVM_EXIT_FAIL_ENTRY => format!("KVM: entry failure: hw_entry_failure_reason={:#x}", (*id).__bindgen_anon_1.fail_entry.hardware_entry_failure_reason),
                    KVM_EXIT_INTERNAL_ERROR => format!("KVM: internal error exit: suberror = {:#x}", (*id).__bindgen_anon_1.internal.suberror),
                    KVM_EXIT_SHUTDOWN => format!("KVM: receive shutdown command"),
                    KVM_EXIT_DEBUG => return Err(Error::KVMDebug),
                    _ => format!("KVM: unhandled exit: exit_reason = {:#x}", (*id).exit_reason)
                };

                return Err(Error::Protocol(err));
            }
        }

        return Ok(Return::Continue);
    }
}
