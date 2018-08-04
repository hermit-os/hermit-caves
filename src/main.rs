#![allow(dead_code)]
#![allow(non_camel_case_types)]
#![allow(non_upper_case_globals)]
#![allow(non_snake_case)]

extern crate libc;
extern crate memmap;
extern crate elf;
extern crate inotify;
extern crate byteorder;
extern crate raw_cpuid;
extern crate rand;

#[macro_use]
extern crate bitflags;

#[macro_use]
extern crate chan;
extern crate chan_signal;

#[macro_use]
extern crate nix;

#[macro_use]
extern crate log;
extern crate env_logger;

mod hermit;

use std::env;
use std::process;

use hermit::Isle;
use hermit::IsleParameter;
use hermit::qemu::QEmu;
use hermit::multi::Multi;
use hermit::uhyve::Uhyve;
use hermit::error::Result;

fn create_isle(path: Option<String>, specs: IsleParameter) -> Result<()> {
    let mut isle: Box<Isle> = match specs {
        IsleParameter::QEmu { mem_size, num_cpus, additional} => Box::new(QEmu::new(path, mem_size, num_cpus, additional)?),
        IsleParameter::UHyve{ mem_size, num_cpus, additional } => Box::new(Uhyve::new(path, mem_size, num_cpus, additional)?),
        IsleParameter::Multi{ num_cpus } => Box::new(Multi::new(0, path, num_cpus)?)
    };

    isle.wait_until_available()?;
    isle.run()?;

    Ok(())
}

fn main() {
    env_logger::init();
    let verbose = IsleParameter::parse_bool("HERMIT_VERBOSE", false);
    unsafe { hermit::verbose = verbose; }

    let path = env::args().nth(1);
    if let Err(e) = create_isle(path, IsleParameter::from_env()) {
        eprintln!("Error: {}", e);
        process::exit(1);
    }
}
