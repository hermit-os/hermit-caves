use std::os::unix::io::RawFd;
use std::ffi::CString;
use std::sync::mpsc;
use std::thread;

use rand::prelude::*;
use nix::sys::eventfd;
use nix::sys::stat::Mode;
use nix::fcntl;
use nix::unistd;
use nix::errno::Errno;
use nix::poll;
use nix;
use libc;

use hermit::uhyve;
use hermit::error::*;
use super::kvm::*;
use super::net_if::*;

const UHYVE_IRQ: u32 = 11;

ioctl_write_ptr!(tunsetiff, b'T', 202, i32);

pub struct NetworkInterface {
    fd: RawFd,
    mac: [u8; 18],
    wait_sender: Option<mpsc::SyncSender<()>>,
    vmfd: RawFd
}

impl NetworkInterface {
    fn attach(dev: &str) -> Result<RawFd> {
        if dev.starts_with("@") {
            let fd = dev[1..].parse::<RawFd>().map_err(|_| Error::NetworkInterface)?;

            let flag = fcntl::OFlag::O_NONBLOCK;
            fcntl::fcntl(fd, fcntl::F_SETFL(flag)).map_err(|_| Error::NetworkInterface)?;
            return Ok(fd)
        }
        
        let fd = fcntl::open("/dev/net/tun", fcntl::OFlag::O_RDWR | fcntl::OFlag::O_NONBLOCK, Mode::empty())
            .map_err(|_| Error::NetworkInterface)?;

        let mut ifr: ifreq = unsafe { ::std::mem::zeroed() };
        ifr.ifr_ifru.ifru_flags = (IFF_TAP | IFF_NO_PI) as i16;
        if dev.len() > IFNAMSIZ as usize {
            return Err(Error::NetworkInterface)
        }

        let dev_c = CString::new(dev).unwrap();
        unsafe { libc::strncpy(ifr.ifr_ifrn.ifrn_name.as_mut_ptr(), dev_c.as_ptr(), IFNAMSIZ as usize); }

        if let Err(_) = unsafe { tunsetiff(fd, &mut ifr as *mut _ as *mut _) } {
            let _ = unistd::close(fd);
            return Err(Error::NetworkInterface)
        }

        if unsafe { libc::strncmp(ifr.ifr_ifrn.ifrn_name.as_ptr(), dev_c.as_ptr(), IFNAMSIZ as usize) } != 0 {
            let _ = unistd::close(fd);
            return Err(Error::NetworkInterface)
        }

        let buf: [u8; 0] = [];
        match unistd::write(fd, &buf) {
            Err(e) => match e {
                nix::Error::Sys(errno) => match errno {
                    Errno::EIO => {
                        let _ = unistd::close(fd);
                        return Err(Error::NetworkInterface)
                    },
                    _ => {}
                },
                _ => {}
            },
            Ok(_) => {}
        }

        Ok(fd)
    }

    pub fn new(vmfd: RawFd, dev: &str, mac: &Option<String>) -> Result<NetworkInterface> {
        let mut nwif = NetworkInterface {
            fd: NetworkInterface::attach(dev)?,
            mac: [0; 18],
            wait_sender: None,
            vmfd: vmfd
        };

        nwif.set_mac(mac);
        nwif.check()?;

        Ok(nwif)
    }

    fn set_mac(&mut self, mac: &Option<String>) {
        if let Some(mac_addr) = mac {
            let mac = mac_addr.chars();
            let mut i = 0;
            let mut s = 0;
            for m in mac {
                if m.is_digit(16) {
                    i += 1;
                } else if m == ':' {
                    s += 1;
                    if i / 2 != s {
                        break
                    }
                } else {
                    s = -1;
                }
            }

            if i != 12 || s != 5 {
                debug!("Malformed mac address: {}", mac_addr);
            } else {
                self.mac[..17].copy_from_slice(&mac_addr.as_bytes());
                self.mac[17] = 0;
                return
            }
        }

        let mut rng = thread_rng();
        let mut guest_mac: [u8; 6] = rng.gen();

        guest_mac[0] &= 0xfe;
        guest_mac[0] |= 0x02;

        let mac_addr = format!("{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
            guest_mac[0], guest_mac[1], guest_mac[2], guest_mac[3], guest_mac[4], guest_mac[5]);

        self.mac[..17].copy_from_slice(&mac_addr.as_bytes());
        self.mac[17] = 0;
    }

    fn wait_for_packet(wait_receiver: mpsc::Receiver<()>, netfd: RawFd, efd: RawFd) {
        let pollfd = poll::PollFd::new(netfd, poll::EventFlags::POLLIN);

        loop {
            match poll::poll(&mut [pollfd], -1000) {
                Err(e) => match e {
                    nix::Error::Sys(errno) => match errno {
                        Errno::EINTR => continue,
                        _ => break
                    },
                    _ => break
                },
                Ok(_) => {
                    let event_counter: u64 = 1;
                    unsafe { libc::write(efd, (&event_counter) as *const _ as *const libc::c_void, ::std::mem::size_of::<u64>()); }
                    let _ = wait_receiver.recv();
                }
            }
        }
    }

    pub fn check(&mut self) -> Result<()> {
        if self.wait_sender.is_none() {
            let mut irqfd = kvm_irqfd::default();

            let efd = eventfd::eventfd(0, eventfd::EfdFlags::empty()).unwrap();
            irqfd.fd = efd as u32;
            irqfd.gsi = UHYVE_IRQ;

            unsafe {
                uhyve::ioctl::irqfd(self.vmfd, (&mut irqfd) as *mut kvm_irqfd)
                    .map_err(|_| Error::IOCTL(NameIOCTL::IRQFD))?;
            }

            let (tx, rx) = mpsc::sync_channel(0);
            self.wait_sender = Some(tx);

            let netfd = self.fd;

            thread::spawn(move || {
                NetworkInterface::wait_for_packet(rx, netfd, efd)
            });
        }

        Ok(())
    }

    pub fn notify(&self) {
        if let Some(tx) = &self.wait_sender {
            let _ = tx.send(());
        }
    }

    pub fn get_netfd(&self) -> RawFd {
        self.fd
    }

    pub fn get_mac(&self) -> &[u8] {
        &self.mac
    }
}
