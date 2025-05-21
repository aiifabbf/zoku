use std::ffi::CString;
use std::fs::remove_file;
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::net::UnixListener;
use std::path::Path;
use std::str::from_utf8;

use nix::poll::{PollFd, PollFlags, PollTimeout, poll};
use nix::pty::{ForkptyResult, forkpty};
use nix::unistd::{execvp, read, write};

pub fn main(bind: impl AsRef<Path>, argv: &[CString]) {
    let listener = UnixListener::bind(bind.as_ref()).expect("address already in use");
    let (stdin, _addr) = listener.accept().unwrap();
    let stdio: OwnedFd = stdin.into();

    match unsafe { forkpty(None, None).unwrap() } {
        ForkptyResult::Parent { child: _, master } => {
            loop {
                let mut fds = [
                    PollFd::new(master.as_fd(), PollFlags::POLLIN),
                    PollFd::new(stdio.as_fd(), PollFlags::POLLIN),
                ];

                let _ = poll(&mut fds, PollTimeout::NONE).unwrap();

                if fds[0].any().unwrap() {
                    let mut buffer = [0; 1024];
                    let n = read(master.as_fd(), &mut buffer).unwrap();
                    if n > 0 {
                        dbg!("received from master: ", from_utf8(&buffer[..n]));
                        write(stdio.as_fd(), &buffer[..n]).unwrap();
                    } else {
                        dbg!("slave is closed");
                        break;
                    }
                }
                if fds[1].any().unwrap() {
                    let mut buffer = [0; 1024];
                    let n = read(stdio.as_fd(), &mut buffer).unwrap();
                    if n > 0 {
                        dbg!("sending to master: ", from_utf8(&buffer[..n]));
                        write(master.as_fd(), &buffer[..n]).unwrap();
                    } else {
                        unreachable!("should not receive stdin close under -icanon");
                    }
                }
            }

            remove_file(bind.as_ref()).unwrap();
        }
        ForkptyResult::Child => {
            let sh = CString::new("/bin/sh".as_bytes()).unwrap();
            let path = argv.get(0).unwrap_or(&sh);
            dbg!("execvp {} {}", argv);
            execvp(&path, &argv).unwrap();
        }
    }
}
