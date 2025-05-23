use std::io::{Read, Write, stdin, stdout};
use std::os::fd::AsFd;
use std::os::unix::net::UnixStream;
use std::path::Path;

use nix::poll::{PollFd, PollFlags, PollTimeout, poll};
use nix::sys::termios::{LocalFlags, SetArg, tcgetattr, tcsetattr};
use nix::unistd::{read, write};

pub fn main(path: &Path) {
    let master = UnixStream::connect(path).expect("cannot find server");

    let mut stdin = stdin();
    let old_tty = tcgetattr(stdin.as_fd()).unwrap();
    let mut tty = old_tty.clone();
    tty.local_flags.set(LocalFlags::ECHO, false);
    tty.local_flags.set(LocalFlags::ICANON, false);
    tty.local_flags.set(LocalFlags::ISIG, false);
    dbg!("set -echo -icanon -isig");
    stdout().flush().unwrap();
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
                stdout().write_all(&buffer[..n]).unwrap();
                stdout().flush().unwrap();
            } else {
                dbg!("remote is closed");
                break;
            }
        }
        if fds[1].any().unwrap() {
            let mut buffer = [0; 1024];
            let n = stdin.read(&mut buffer).unwrap();
            if n > 0 {
                write(master.as_fd(), &buffer[..n]).unwrap();
            } else {
                dbg!("master is closed");
                break;
            }
        }
    }

    dbg!("reset tty");
    tcsetattr(stdin.as_fd(), SetArg::TCSAFLUSH, &old_tty).unwrap();
}
