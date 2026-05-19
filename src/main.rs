use std::{
    env::{args, args_os},
    ffi::CString,
    os::unix::{ffi::OsStrExt, net::UnixStream},
    path::Path,
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
            if let Some(_) = UnixStream::connect(path).ok() {
                eprintln!("zoku: Another session is already running on {}", path);
                return;
            }
            let pid = unsafe { fork() };
            if pid != 0 {
                while let None = UnixStream::connect(path).ok() {
                    unsafe {
                        sleep(0);
                    }
                }
                client::main(Path::new(path));
            } else {
                daemon().unwrap();
                server::main(Path::new(path), &argv);
            }
        }
        [Some("attach"), Some(path)] => client::main(Path::new(path)),
        [Some("serve"), Some(path)] => {
            let argv: Vec<_> = args_os()
                .skip(3)
                .map(|arg| CString::new(arg.as_bytes()).unwrap())
                .collect();
            server::main(Path::new(path), &argv);
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
