use std::net::TcpStream;
use std::env;
use std::io::{Write, Read, Cursor};
use std::io;
use std::mem;
use std::ffi::CStr;
use std::{thread, time};

use byteorder::{WriteBytesExt, NativeEndian};
use nix::unistd::{write, read};
use nix::fcntl::{open, OFlag};
use nix::sys::stat::Mode;
use libc;

use hermit::proto;
use hermit::proto::Packet;
use hermit::error::{Error, Result};

const HERMIT_MAGIC: u32 = 0x7E317;

#[derive(Debug)]
pub struct Socket {
    stream: Option<TcpStream>, 
    port: u16,
}

impl Socket {
    pub fn new(port: u16) -> Socket {
        Socket { stream: None, port: port }
    }

    fn write_header(buf: &mut [u8]) -> io::Result<()> {
        let mut cur = Cursor::new(buf);
        cur.write_u32::<NativeEndian>(HERMIT_MAGIC)?;
        // send all arguments (skip first)
        cur.write_u32::<NativeEndian>(env::args().count() as u32 - 1)?;
        for key in env::args().skip(1) {
            cur.write_u32::<NativeEndian>(key.len() as u32 + 1)?;
            cur.write(key.as_bytes())?;
            cur.write_u8(b'\0')?;
        }

        // send the environment
        cur.write_u32::<NativeEndian>(env::vars().count() as u32)?;
        for (val,key) in env::vars() {
            let tmp = format!("{}={}", val, key);
            cur.write_u32::<NativeEndian>(tmp.len() as u32)?;
            cur.write(tmp.as_bytes())?;
        }

        Ok(())
    }

    pub fn connect(&mut self) -> Result<()> {
        // prepare the initializing struct
        let length: usize = mem::size_of::<i32>() * 3 +
                            env::args().skip(1).map(|x| mem::size_of::<i32>() + x.len() + 1).sum::<usize>() +
                            env::vars().map(|(x,y)| mem::size_of::<i32>() + x.len() + y.len() + 1).sum::<usize>();

        let mut buf = vec![0u8;length];
        Socket::write_header(&mut buf).map_err(|_| Error::ProxyConnect)?;

        for _ in 0..10 {
            match TcpStream::connect(("127.0.0.1", self.port)) {
                Ok(mut s) => {
                    self.stream = Some(s);
                    let mut stream = self.stream()?;
                    return match stream.write(&buf) {
                        Ok(_) => {
                            debug!("Connected to {}", stream.peer_addr().unwrap());
                            debug!("Transmitted environment and arguments with length {}", length);
                            Ok(())
                        }
                        Err(_) => Err(Error::ProxyConnect)
                    }
                },
                Err(_) => thread::sleep(time::Duration::from_millis(10))
            }
        }

        Err(Error::ProxyConnect)
    }

    pub fn stream(&self) -> Result<&TcpStream> {
        self.stream.as_ref().ok_or(Error::InternalError)
    }

    fn handle_packet(s: &TcpStream, packet: Packet) -> io::Result<bool> {
        let mut stream = s;
        match packet {
            Packet::Exit { arg: _ } => return Ok(true),
            Packet::Write { fd, buf } => {
                let res = match write(fd, &buf) {
                    Ok(r) => r as i64,
                    Err(_) => -1
                };
                if fd > 2 {
                    stream.write_i64::<NativeEndian>(res)?;
                }
            },
            Packet::Open { name, mode, flags } => {
                let c_str = unsafe { CStr::from_bytes_with_nul_unchecked(&name) };
                let fd = match open(c_str, OFlag::from_bits_truncate(flags), Mode::from_bits_truncate(mode)) {
                    Ok(r) => r,
                    Err(_) => -1
                };
                stream.write_i32::<NativeEndian>(fd)?;
            },
            Packet::Close { fd } => {
                let res: i32 = match fd {
                    n if n > 2 => unsafe { libc::close(fd) },
                    _ => 0
                };

                stream.write_i32::<NativeEndian>(res)?;
            },
            Packet::Read { fd, len } => {
                let mut tmp: Vec<u8> = vec![0; len as usize];
                let res = match read(fd, &mut tmp) {
                    Ok(r) => r as i64,
                    Err(_) => -1
                };

                stream.write_i64::<NativeEndian>(res)?;

                if res > 0 {
                    stream.write_all(&tmp[0 .. (res as usize)])?;
                }
            },
            Packet::LSeek { fd, offset, whence } => {
                let res = unsafe { libc::lseek(fd, offset, whence) };
                stream.write_i64::<NativeEndian>(res)?;
            }
        };

        return Ok(false);
    }

    pub fn run(&mut self) -> Result<()> {
        debug!("Initializing protocol state machine");
        let mut state = proto::State::Id;
        let mut stream = self.stream()?;

        let mut cur = Cursor::new(vec![]);
        let mut buf = [0u8; 4096];
        'main: loop {
            debug!("Attempt read");
            let nread = stream.read(&mut buf).unwrap();

            let old_position = cur.position();
            let end = cur.get_ref().len() as u64;

            cur.set_position(end);
            cur.write_all(&buf[0..nread]).unwrap();
            cur.set_position(old_position);

            debug!("Got message with {} bytes: {:?}", nread, &buf[0 .. nread]);

            let mut old_position = cur.position();

            loop {
                state = state.read_in(&mut cur)?;

                if let proto::State::Finished(packet) = state {
                    match Socket::handle_packet(stream, packet) {
                        Ok(true) => break 'main,
                        Err(_) => return Err(Error::ProxyConnect),
                        _ => {}
                    }

                    state = proto::State::Id;
                }

                if cur.position() == old_position {
                    break; // we need more data
                } else {
                    old_position = cur.position();
                }
            }
        }
        Ok(())
    }
}
