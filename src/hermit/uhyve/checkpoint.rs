use std::str::Lines;
use std::fs::File;
use std::io::{Read, Write};

use hermit::error::*;
use hermit::utils;
use super::vcpu::vcpu_state;

#[repr(C)]
#[derive(Default)]
pub struct CheckpointConfig {
    pub num_cpus: u32,
    pub mem_size: isize,
    pub checkpoint_number: u32,
    pub elf_entry: u64,
    pub full: bool
}

#[derive(Default)]
pub struct CheckpointData {
    pub config: CheckpointConfig,
    pub cpu_states: Vec<vcpu_state>
}

pub struct FileCheckpoint {
    data: CheckpointData
}

impl CheckpointConfig {
    fn check_line<'a>(lines: &'a mut Lines, start: &str) -> Result<&'a str> {
        lines.find(|&l| l.starts_with(start)).map(|l| &l[start.len() ..]).ok_or(Error::InvalidCheckpoint)
    }

    pub fn from(data: &str) -> Result<CheckpointConfig> {
        let cores = CheckpointConfig::check_line(&mut data.lines(), "number of cores: ")?
            .parse().map_err(|_| Error::InvalidCheckpoint)?;

        //println!("-- {}", CheckpointConfig::check_line(&mut data.lines(), "memory size: 0x")?);
        let size = isize::from_str_radix(CheckpointConfig::check_line(&mut data.lines(), "memory size: 0x")?, 16)
            .map_err(|_| Error::InvalidCheckpoint)?;

        let num = CheckpointConfig::check_line(&mut data.lines(), "checkpoint number: ")?
            .parse().map_err(|_| Error::InvalidCheckpoint)?;

        let elf_entry = u64::from_str_radix(CheckpointConfig::check_line(&mut data.lines(), "entry point: 0x")?, 16)
            .map_err(|_| Error::InvalidCheckpoint)?;

        let full = CheckpointConfig::check_line(&mut data.lines(), "full checkpoint: ")?
            .parse::<u32>().map(|x| x != 0).map_err(|_| Error::InvalidCheckpoint)?;

        Ok(CheckpointConfig {
            num_cpus: cores,
            mem_size: size,
            checkpoint_number: num,
            elf_entry: elf_entry,
            full: full
        })
    }

    pub fn get_num_cpus(&self) -> u32 {
        self.num_cpus
    }

    pub fn get_mem_size(&self) -> u64 {
        self.mem_size as u64
    }

    pub fn get_elf_entry(&self) -> u64 {
        self.elf_entry
    }

    pub fn get_checkpoint_number(&self) -> u32 {
        self.checkpoint_number
    }

    pub fn get_full(&self) -> bool {
        self.full
    }
}

impl FileCheckpoint {
    pub fn load() -> Result<FileCheckpoint> {
        let mut chk_file = File::open("checkpoint/chk_config.txt")
            .map_err(|_| Error::NoCheckpointFile)?;

        let mut contents = String::new();
        chk_file.read_to_string(&mut contents).map_err(|_| Error::InvalidCheckpoint)?;
        let data = CheckpointData {
            config: CheckpointConfig::from(&contents)?,
            cpu_states: Vec::new()
        };

        let mut file_chk = FileCheckpoint { data: data };
        file_chk.read_cpu_states()?;

        Ok(file_chk)
    }

    pub fn new(config: CheckpointConfig) -> FileCheckpoint {
        FileCheckpoint {
            data: CheckpointData {
                config: config,
                cpu_states: Vec::new()
            }
        }
    }

    pub fn save(&self) -> Result<()> {
        let mut chk_file = File::create("checkpoint/chk_config.txt")
            .map_err(|_| Error::WriteCheckpoint)?;
        
        let content = self.data.config.to_string();
        chk_file.write_all(content.as_bytes()).map_err(|_| Error::WriteCheckpoint)?;

        self.write_cpu_states()?;
        Ok(())
    }

    fn read_cpu_states(&mut self) -> Result<()> {
        for cpuid in 0 .. self.data.config.get_num_cpus() {
            let file_name = format!("checkpoint/chk{}_core{}.dat", self.data.config.get_checkpoint_number(), cpuid);
            let mut file = File::open(&file_name).map_err(|_| Error::InvalidCheckpoint)?;
            
            let mut cpu_state = vcpu_state::default();
            file.read_exact(unsafe { utils::any_as_u8_mut_slice(&mut cpu_state) }).map_err(|_| Error::InvalidCheckpoint)?;
            self.data.cpu_states.push(cpu_state);
        }
        Ok(())
    }

    fn write_cpu_states(&self) -> Result<()> {
        for cpuid in 0 .. self.data.cpu_states.len() {
            let file_name = format!("checkpoint/chk{}_core{}.dat", self.data.config.get_checkpoint_number(), cpuid);
            let mut file = File::create(&file_name).map_err(|_| Error::WriteCheckpoint)?;
            
            let state = &self.data.cpu_states[cpuid];
            file.write_all(unsafe { utils::any_as_u8_slice(state) }).map_err(|_| Error::WriteCheckpoint)?;
        }
        Ok(())
    }

    pub fn get_mem_file_path(checkpoint_num: u32) -> String {
        format!("checkpoint/chk{}_mem.dat", checkpoint_num)
    }

    pub fn get_config(&self) -> &CheckpointConfig {
        &self.data.config
    }

    pub fn get_cpu_states(&mut self) -> &mut Vec<vcpu_state> {
        &mut self.data.cpu_states
    }
}

impl ToString for CheckpointConfig {
    #[inline]
    fn to_string(&self) -> String {
        format!("number of cores: {}\n\
            memory size: {:#x}\n\
            checkpoint number: {}\n\
            entry point: {:#x}\n\
            full checkpoint: {}",
            self.num_cpus,
            self.mem_size,
            self.checkpoint_number,
            self.elf_entry,
            self.full as i32
        )
    }
}
