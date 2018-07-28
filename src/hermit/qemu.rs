use std::process::{Stdio, Child, Command};
use std::path::Path;
use std::process::{ChildStdout, ChildStderr};
use std::env;

use nix::sys::signal::{kill, SIGINT};
use nix::unistd::Pid;

use hermit::{Isle, IsleParameterQEmu};
use hermit::utils;
use hermit::error::*;
use hermit::socket::Socket;
use hermit;

const PIDNAME: &'static str = "/tmp/hpid-XXXXXX";
const TMPNAME: &'static str = "/tmp/hermit-XXXXXX";

#[derive(Debug)]
pub struct QEmu {
    socket: Socket,
    child: Child,
    stdout: ChildStdout,
    stderr: ChildStderr,
    tmp_file: utils::TmpFile,
    pid_file: utils::TmpFile,
}

impl QEmu {
    pub fn new(path: Option<String>, mem_size: u64, num_cpus: u32, additional: IsleParameterQEmu) -> Result<QEmu> {
        let tmpf = utils::TmpFile::create(TMPNAME)?;
        let pidf = utils::TmpFile::create(PIDNAME)?;
        
        let port = if additional.port == 0 || additional.port >= u16::max_value() {
            hermit::BASE_PORT
        } else {
            additional.port
        };

        debug!("port number: {}", port);

        let mut child = QEmu::start_with(&path.ok_or(Error::FileMissing)?,
            port, mem_size, num_cpus, additional, tmpf.get_path(), pidf.get_path()).spawn()
            .map_err(|_| Error::MissingQEmuBinary)?;
        let stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();
        let socket = Socket::new(port);

        Ok(QEmu {
            socket: socket,
            child: child,
            stdout: stdout,
            stderr: stderr,
            tmp_file: tmpf,
            pid_file: pidf,
        })
    }
    
    pub fn start_with(path: &str, port: u16, mem_size: u64, num_cpus: u32, add: IsleParameterQEmu, tmp_file: &Path, pid_file: &Path) -> Command {
        let mut hostfwd = format!("user,hostfwd=tcp:127.0.0.1:{}-:{}", port, hermit::BASE_PORT);
        let monitor_str = format!("telnet:127.0.0.1:{},server,nowait", port+1);
        let chardev = format!("file,id=gnc0,path={}", &tmp_file.display());
        let freq = format!("\"-freq{} -proxy\"", utils::cpufreq().unwrap().to_string());
        let num_cpus = num_cpus.to_string();
        let mem_size = format!("{}B", mem_size);

        if add.app_port != 0 {
            hostfwd = format!("{},hostfwd=tcp::{}-:{}", hostfwd, add.app_port, add.app_port);
        }

        let mut exe = env::current_exe().unwrap();
        exe.pop();
        exe.push("ldhermit.elf");
        let kernel = exe.to_str().unwrap();

        let mut args: Vec<&str> = vec![
            "-daemonize",
            "-display", "none",
            "-smp", &num_cpus,
            "-m", &mem_size,
            "-pidfile", pid_file.to_str().unwrap(),
            "-net", "nic,model=rtl8139",
            "-net", &hostfwd,
            "-chardev", &chardev,
            "-device", "pci-serial,chardev=gnc0",
            "-kernel", kernel,
            "-initrd", path,
            "-append", &freq,
            "-no-acpi"];

        if add.use_kvm {
            args.push("-machine");
            args.push("accel=kvm");
            args.push("-cpu");
            args.push("host");
        }

        if add.monitor {
            args.push("-monitor");
            args.push(&monitor_str);
        }

        if add.should_debug {
            args.push("-s");
        }

        if add.capture_net {
            args.push("-net");
            args.push("dump");
        }

        debug!("Execute {} with {}", add.binary, args.join(" "));

        let mut cmd = Command::new(add.binary);

        cmd.args(args).stdout(Stdio::piped()).stderr(Stdio::piped());

        cmd
    }

    /*pub fn qemu_log(&mut self) -> (String, String) {
        let mut stderr = String::new();
        let mut stdout = String::new();

        self.stdout.read_to_string(&mut stdout);
        self.stderr.read_to_string(&mut stderr);

        (stdout, stderr)
    }*/
}

impl Isle for QEmu {
    fn num(&self) -> u8 { 0 }
    fn log_file(&self) -> Option<&Path> {
        Some(self.tmp_file.get_path())
    }

    fn run(&mut self) -> Result<()> {
        self.socket.connect()?;
        self.socket.run()?;

        Ok(())
    }

    fn stop(&mut self) -> Result<()> {
        let mut id_str = String::new();
        self.pid_file.read_to_string(&mut id_str).ok();
        id_str.pop();

        if let Ok(id) = id_str.parse::<i32>() {
            if id >= 0 {
                let _ = kill(Pid::from_raw(id), SIGINT);
            }
        }

        Ok(())
    }

    fn output(&self) -> String {
        let mut content = String::new();
        match self.tmp_file.read_to_string(&mut content) {
            Ok(_) => content,
            Err(_) => {
                debug!("Could not read kernel log");
                "".into()
            }
        }
    }
}

impl Drop for QEmu {
    fn drop(&mut self) {
        let _ = self.stop();

        if hermit::is_verbose() {
            println!("Dump kernel log:");
            println!("================");
            for line in self.output().lines() {
                println!("{}", line);
            }
        }
    }
}
