use std::fs::File;
use std::path::{Path, PathBuf};
use std::io::{Write, Read};

use hermit::Isle;
use hermit::error::*;
use hermit::socket::Socket;
use hermit;

pub struct Multi {
    num: u8,
    socket: Socket,
    logfile: PathBuf
}

impl Multi {
    pub fn new(num: u8, path: Option<String>, num_cpus: u32) -> Result<Multi> {
        let cpu_path = format!("/sys/hermit/isle{}/path", num);
        let bin_path = format!("/sys/hermit/isle{}/cpus", num);

        // request a new isle, enforce close
        {
            let mut path_file = File::create(&bin_path)
                .map_err(|_| Error::InvalidFile(bin_path.clone()))?;

            let mut cpus_file = File::create(&cpu_path)
                .map_err(|_| Error::InvalidFile(cpu_path.clone()))?;
            
            let cpus = num_cpus.to_string();

            path_file.write_all(path.ok_or(Error::FileMissing)?.as_bytes())
                .map_err(|_| Error::InvalidFile(cpu_path.clone()))?;
            
            cpus_file.write_all(cpus.as_bytes())
                .map_err(|_| Error::InvalidFile(bin_path))?;
        }

        // check the result
        let mut path_file = File::create(&cpu_path)
            .map_err(|_| Error::InvalidFile(cpu_path.clone()))?;

        let mut result = String::new();
        
        path_file.read_to_string(&mut result)
            .map_err(|_| Error::InvalidFile(cpu_path.clone()))?;
        
        if result.parse::<i32>().map_err(|_| Error::InvalidFile(cpu_path))? == -1 {
            return Err(Error::MultiIsleFailed);
        }

        let logfile = PathBuf::from(format!("/sys/hermit/isle{}/log", num));

        Ok(Multi { num: num, socket: Socket::new(hermit::BASE_PORT), logfile: logfile })
    }
}

impl Isle for Multi {
    fn num(&self) -> u8 {
        self.num
    }

    fn log_file(&self) -> Option<&Path> {
        return Some(self.logfile.as_path());
    }

    fn run(&mut self) -> Result<()> {
        self.socket.connect()?;
        self.socket.run()?;

        Ok(())
    }

    fn stop(&mut self) -> Result<()> {
        debug!("Stop the HermitIsle");

        let cpu_path = format!("/sys/hermit/isle{}/path", self.num);

        let mut cpus_file = File::create(&cpu_path)
            .map_err(|_| Error::InvalidFile(cpu_path.clone()))?;

        cpus_file.write("-1".as_bytes())
            .map_err(|_| Error::InvalidFile(cpu_path))?;
    
        Ok(())
    }

    fn output(&self) -> String {
        "".into()
    }
}
