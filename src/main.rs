use std::env::args_os;
use std::ffi::CString;
use std::io::{Read, stdin};
use std::os::fd::AsFd;
use std::os::unix::ffi::OsStrExt;
use std::str::from_utf8;

use nix::poll::{PollFd, PollFlags, PollTimeout, poll};
use nix::pty::{ForkptyResult, forkpty};
use nix::unistd::{execve, read, write};

fn main() {
    match unsafe { forkpty(None, None).unwrap() } {
        ForkptyResult::Parent { child, master } => loop {
            let mut stdin = stdin();
            let mut fds = [
                PollFd::new(master.as_fd(), PollFlags::POLLIN),
                PollFd::new(stdin.as_fd(), PollFlags::POLLIN),
            ];

            let _ = poll(&mut fds, PollTimeout::NONE).unwrap();

            if fds[0].any().unwrap() {
                let mut buffer = [0; 1024];
                let n = read(master.as_fd(), &mut buffer).unwrap();
                if n > 0 {
                    dbg!("received from master: ", from_utf8(&buffer[..n]));
                } else {
                    dbg!("slave is closed");
                    break;
                }
            }
            if fds[1].any().unwrap() {
                let mut buffer = [0; 1024];
                let n = stdin.read(&mut buffer).unwrap();
                if n > 0 {
                    dbg!("sending to master: ", from_utf8(&buffer[..n]));
                    write(master.as_fd(), &buffer[..n]).unwrap();
                } else {
                    dbg!("master is closed");
                    break;
                }
            }
        },
        ForkptyResult::Child => {
            let path = CString::new(args_os().skip(1).next().unwrap().as_bytes()).unwrap();
            let argv: Vec<_> = args_os()
                .skip(1)
                .map(|arg| CString::new(arg.as_bytes()).unwrap())
                .collect();
            let env = [CString::new("TERM=xterm").unwrap()];
            execve(&path, &argv, &env).unwrap();
        }
    }
}
