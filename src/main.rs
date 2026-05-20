use std::{
    env::{args, args_os},
    ffi::CString,
    os::unix::{
        ffi::OsStrExt,
        net::{UnixListener, UnixStream},
    },
};

use nix::libc::{close, fork, setsid, sleep, umask};

mod client;
mod server;

fn daemon() -> Option<()> {
    unsafe {
        setsid();
        umask(0);
        close(0);
        close(1);
        close(2);
    }
    Some(())
}

fn main() {
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
                } else {
                    daemon().unwrap();
                    server::main(listener, &argv);
                }
            } else {
                eprintln!("zoku: another session is already running on {}", path);
            }
        }
        [Some("attach"), Some(path)] => {
            if let Some(master) = UnixStream::connect(path).ok() {
                client::main(master);
            } else {
                eprintln!("zoku: no session is running on {}", path);
            }
        }
        [Some("serve"), Some(path)] => {
            let argv: Vec<_> = args_os()
                .skip(3)
                .map(|arg| CString::new(arg.as_bytes()).unwrap())
                .collect();
            if let Some(listener) = UnixListener::bind(path).ok() {
                server::main(listener, &argv);
            } else {
                eprintln!("zoku: another session is already running on {}", path);
            }
        }
        _ => {
            println!(
                "Usage:
    zoku new path program
    zoku attach path
    zoku serve path program"
            );
        }
    }
}
