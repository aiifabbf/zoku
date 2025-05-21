use std::env::args_os;
use std::ffi::CString;
use std::io::{Read, Write, stdin, stdout};
use std::os::fd::AsFd;
use std::os::unix::ffi::OsStrExt;

use nix::poll::{PollFd, PollFlags, PollTimeout, poll};
use nix::pty::{ForkptyResult, forkpty};
use nix::sys::termios::{LocalFlags, SetArg, tcgetattr, tcsetattr};
use nix::unistd::{execve, read, write};

fn main() {
    match unsafe { forkpty(None, None).unwrap() } {
        ForkptyResult::Parent { child: _, master } => {
            let mut stdin = stdin();
            let old_tty = tcgetattr(stdin.as_fd()).unwrap();
            let mut tty = old_tty.clone();
            tty.local_flags.set(LocalFlags::ECHO, false);
            tty.local_flags.set(LocalFlags::ICANON, false);
            tty.local_flags.set(LocalFlags::ISIG, false);
            dbg!("set -echo -icanon -isig");
            tcsetattr(stdin.as_fd(), SetArg::TCSAFLUSH, &tty).unwrap();

            loop {
                let mut fds = [
                    PollFd::new(master.as_fd(), PollFlags::POLLIN),
                    PollFd::new(stdin.as_fd(), PollFlags::POLLIN),
                ];

                let _ = poll(&mut fds, PollTimeout::NONE).unwrap();

                if fds[0].any().unwrap() {
                    let mut buffer = [0; 1024];
                    let n = read(master.as_fd(), &mut buffer).unwrap();
                    if n > 0 {
                        // dbg!("received from master: ", from_utf8(&buffer[..n]));
                        stdout().write_all(&buffer[..n]).unwrap();
                        stdout().flush().unwrap();
                    } else {
                        dbg!("slave is closed");
                        break;
                    }
                }
                if fds[1].any().unwrap() {
                    let mut buffer = [0; 1024];
                    let n = stdin.read(&mut buffer).unwrap();
                    if n > 0 {
                        // dbg!("sending to master: ", from_utf8(&buffer[..n]));
                        write(master.as_fd(), &buffer[..n]).unwrap();
                    } else {
                        unreachable!("should not receive stdin close under -icanon");
                    }
                }
            }

            dbg!("reset tty");
            tcsetattr(stdin.as_fd(), SetArg::TCSAFLUSH, &old_tty).unwrap();
        }
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
