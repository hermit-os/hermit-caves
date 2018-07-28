use std::io::{Read, Cursor};
use std::mem;

use byteorder::{ReadBytesExt, NativeEndian};

use hermit::error::*;

const PACKET_LENGTH: &'static [u64] = &[1,3,1,1,2,3];

#[derive(Debug)]
pub enum PartialPacket {
    Exit { arg: i32 },
    Write { fd: i32, len: u64 },
    Open { len: i64 },
    Close { fd: i32 },
    Read { fd: i32, len: u64 },
    LSeek { fd: i32, offset: i64, whence: i32 }
}

impl PartialPacket {
    pub fn from_buf(id: i32, buf: &mut Cursor<Vec<u8>>) -> Result<PartialPacket> {
        Ok(match id {
            0 => PartialPacket::Exit { 
                arg: buf.read_i32::<NativeEndian>().unwrap() 
            },
            1 => PartialPacket::Write {
                fd: buf.read_i32::<NativeEndian>().unwrap(),
                len: buf.read_u64::<NativeEndian>().unwrap()
            },
            2 => PartialPacket::Open {
                len: buf.read_i64::<NativeEndian>().unwrap()
            },
            3 => PartialPacket::Close {
                fd: buf.read_i32::<NativeEndian>().unwrap()
            },
            4 => PartialPacket::Read {
                fd: buf.read_i32::<NativeEndian>().unwrap(),
                len: buf.read_u64::<NativeEndian>().unwrap()
            },
            5 => PartialPacket::LSeek {
                fd: buf.read_i32::<NativeEndian>().unwrap(),
                offset: buf.read_i64::<NativeEndian>().unwrap(),
                whence: buf.read_i32::<NativeEndian>().unwrap()
            },
            _ => return Err(Error::ProxyPacket)
        })
    }

    pub fn additional_size(&self) -> usize {
        match *self {
            PartialPacket::Write { fd: _, len } => len as usize,
            PartialPacket::Open { len } => len as usize + 2 * mem::size_of::<i32>(),
            _ => 0
        }
    }
}

#[derive(Debug)]
pub enum Packet {
    Exit { arg: i32 },
    Write { fd: i32, buf: Vec<u8> },
    Open { name: Vec<u8>, flags: i32, mode: u32 },
    Close { fd: i32 },
    Read { fd: i32, len: u64 },
    LSeek { fd: i32, whence: i32, offset: i64 }
}

impl Packet {
    pub fn from_buf(obj: &PartialPacket, buf: &mut Cursor<Vec<u8>>) -> Packet {

        match *obj {
            PartialPacket::Write { fd, len } => {
                let mut content = vec![0; len as usize];
                let _ = buf.read_exact(&mut content);

                Packet::Write { fd: fd, buf: content }
            },
            PartialPacket::Open { len } => {
                let mut name_buf = vec![0; len as usize];
                let _ = buf.read_exact(&mut name_buf);
                name_buf.pop();

                Packet::Open {
                    name: name_buf, 
                    flags: buf.read_i32::<NativeEndian>().unwrap(), 
                    mode: buf.read_u32::<NativeEndian>().unwrap() 
                }
            },
            PartialPacket::Exit { arg } => Packet::Exit { arg },
            PartialPacket::Close { fd } => Packet::Close { fd },
            PartialPacket::Read { fd, len } => Packet::Read { fd, len },
            PartialPacket::LSeek { fd, whence, offset } => Packet::LSeek { fd, whence, offset }
        }
    }
}

#[derive(Debug)]
pub enum State {
    Id,
    Type { id: i32, len: u64 },
    Partial(PartialPacket),
    Finished(Packet)
}

impl State {
    pub fn read_in(self, buf: &mut Cursor<Vec<u8>>) -> Result<State> {
        let length = buf.get_ref().len() - buf.position() as usize;
        
        Ok(match self {
            State::Id if length >= mem::size_of::<i32>() => {
                let id = buf.read_i32::<NativeEndian>().unwrap();
                State::Type { 
                    id: id, 
                    len: match id as usize {
                        x @ 0...5 => PACKET_LENGTH[x],
                        _ => 0
                    }
                }
            },
            State::Type { id, len } if length >= (len as usize) * mem::size_of::<i32>() => {
                State::Partial(PartialPacket::from_buf(id, buf)?)
            },
            State::Partial(ref packet) if length >= packet.additional_size()  => {
                State::Finished(Packet::from_buf(packet, buf))
            },
            _ => self
        })
    }
}
