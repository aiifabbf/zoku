use std::{
    env::{args, args_os},
    ffi::CString,
    os::unix::{
        ffi::OsStrExt,
        net::{UnixListener, UnixStream},
    },
    process::ExitCode,
};

use nix::libc::open;
use nix::libc::{O_RDWR, dup2, fork, setsid, sleep, umask};

mod client;
mod server;

fn daemon() -> Option<()> {
    unsafe {
        setsid();
        umask(0);
        // redirect stdin, stdout, stderr
        let fd = open(CString::new("/dev/null").unwrap().as_ptr(), O_RDWR);
        dup2(fd, 0);
        dup2(fd, 1);
        dup2(fd, 2);
    }
    Some(())
}

fn main() -> ExitCode {
    match [args().nth(1).as_deref(), args().nth(2).as_deref()] {
        [Some("new"), Some(path)] => {
            let argv: Vec<_> = args_os()
                .skip(3)
                .map(|arg| CString::new(arg.as_bytes()).unwrap())
                .collect();
            if let Some(listener) = UnixListener::bind(path).ok() {
                let pid = unsafe { fork() };
                if pid != 0 {
                    loop {
                        if let Some(master) = UnixStream::connect(path).ok() {
                            client::main(master);
                            break;
                        } else {
                            // what if server exits too soon?
                            unsafe {
                                sleep(0);
                            }
                        }
                    }
                    ExitCode::SUCCESS
                } else {
                    daemon().unwrap();
                    server::main(listener, &argv);
                    ExitCode::SUCCESS
                }
            } else {
                eprintln!("zoku: another session is already running on {}", path);
                ExitCode::FAILURE
            }
        }
        [Some("attach"), Some(path)] => {
            if let Some(master) = UnixStream::connect(path).ok() {
                client::main(master);
                ExitCode::SUCCESS
            } else {
                eprintln!("zoku: no session is running on {}", path);
                ExitCode::FAILURE
            }
        }
        [Some("serve"), Some(path)] => {
            let argv: Vec<_> = args_os()
                .skip(3)
                .map(|arg| CString::new(arg.as_bytes()).unwrap())
                .collect();
            if let Some(listener) = UnixListener::bind(path).ok() {
                server::main(listener, &argv);
                ExitCode::SUCCESS
            } else {
                eprintln!("zoku: another session is already running on {}", path);
                ExitCode::FAILURE
            }
        }
        _ => {
            println!(
                "Usage:
    zoku new path program
    zoku attach path
    zoku serve path program"
            );
            ExitCode::FAILURE
        }
    }
}
