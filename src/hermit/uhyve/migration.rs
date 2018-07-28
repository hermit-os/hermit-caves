use std::str::FromStr;
use std::net::{TcpListener, TcpStream, SocketAddr, Ipv4Addr};
use std::io::{Read, Write};

use hermit::utils;
use hermit::error::*;
use super::checkpoint::{CheckpointData, CheckpointConfig};
use super::vcpu::vcpu_state;

pub const MIGRATION_PORT: u16 = 1337;

#[derive(Debug, Clone)]
pub enum MigrationType {
    Cold,
    Live
}

impl FromStr for MigrationType {
    type Err = Error;

    fn from_str(s: &str) -> Result<MigrationType> {
        match s {
            "cold" | "COLD" => Ok(MigrationType::Cold),
            "live" | "LIVE" => Ok(MigrationType::Live),
            _ => Err(Error::UnsupportedMigrationType(s.into())),
        }
    }
}

pub struct MigrationClient {
    stream: TcpStream
}

impl MigrationClient {
    pub fn connect(addr: Ipv4Addr) -> Result<MigrationClient> {
        Ok(MigrationClient {
            stream: TcpStream::connect((addr, MIGRATION_PORT)).map_err(|_| Error::MigrationConnection)?
        })
    }

    pub fn send_data(&mut self, buf: &[u8]) -> Result<()> {
        self.stream.write_all(buf).map_err(|_| Error::MigrationStream)
    }
}

pub struct MigrationServer {
    stream: TcpStream,
    data: CheckpointData
}

impl MigrationServer {
    pub fn wait_for_incoming() -> Result<MigrationServer> {
        let listener = TcpListener::bind(SocketAddr::from(([0, 0, 0, 0], MIGRATION_PORT)))
            .map_err(|_| Error::MigrationConnection)?;

        match listener.incoming().next() {
            Some(stream) => {
                let mut mig_server = MigrationServer {
                    stream: stream.map_err(|_| Error::MigrationConnection)?,
                    data: CheckpointData::default()
                };

                let mut cfg = CheckpointConfig::default();
                mig_server.recv_data(unsafe { utils::any_as_u8_mut_slice(&mut cfg) })?;
                debug!("Received meta: {}", ::std::mem::size_of::<CheckpointConfig>());
                mig_server.data.config = cfg;

                Ok(mig_server)
            },
            None => Err(Error::MigrationConnection)
        }
    }

    pub fn recv_cpu_states(&mut self) -> Result<()> {
        for _ in 0 .. self.data.config.get_num_cpus() {
            let mut cpu_state = vcpu_state::default();
            self.recv_data(unsafe { utils::any_as_u8_mut_slice(&mut cpu_state) })?;
            debug!("Received cpu state: {}", ::std::mem::size_of::<vcpu_state>());
            self.data.cpu_states.push(cpu_state);
        }
        Ok(())
    }

    pub fn recv_data(&mut self, buf: &mut [u8]) -> Result<()> {
        self.stream.read_exact(buf).map_err(|_| Error::MigrationStream)
    }

    pub fn get_metadata(&self) -> &CheckpointConfig {
        &self.data.config
    }

    pub fn get_cpu_states(&mut self) -> &mut Vec<vcpu_state> {
        &mut self.data.cpu_states
    }
}
