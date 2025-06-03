use std::io::Write;
use std::os::fd::{AsFd, AsRawFd};
use std::path::Path;
use std::process::exit;

use nix::libc::TIOCGWINSZ;
use nix::pty::Winsize;
use nix::sys::termios::{LocalFlags, SetArg, tcgetattr, tcsetattr};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use tokio::runtime::Runtime;
use tokio::select;
use tokio::signal::unix::{SignalKind, signal};

const BUFFER_SIZE: usize = 4096; // bytes

pub fn main(path: &Path) {
    let old_tty = tcgetattr(std::io::stdin().as_fd()).unwrap();
    let mut tty = old_tty.clone();
    tty.local_flags.set(LocalFlags::ECHO, false);
    tty.local_flags.set(LocalFlags::ICANON, false);
    tty.local_flags.set(LocalFlags::ISIG, false);
    dbg!("set -echo -icanon -isig");
    std::io::stdout().flush().unwrap();
    tcsetattr(std::io::stdin().as_fd(), SetArg::TCSAFLUSH, &tty).unwrap();

    let rt = Runtime::new().unwrap();
    rt.block_on(async {
        let mut master = UnixStream::connect(path).await.expect("cannot find server");
        let mut signals = signal(SignalKind::window_change()).unwrap();
        let mut stdin = tokio::io::stdin();
        let mut stdout = tokio::io::stdout();

        loop {
            let mut master_buffer = [0; BUFFER_SIZE];
            let mut stdin_buffer = [0; BUFFER_SIZE];

            select! {
                biased;
                Ok(n) = stdin.read(&mut stdin_buffer) => {
                    let msg = &stdin_buffer[..n];
                    if n > 0 {
                        master.write_all(msg).await.unwrap();
                        master.flush().await.unwrap();
                    } else {
                        dbg!("master is closed");
                        break;
                    }
                }
                Some(()) = signals.recv() => {
                    let mut ws: Winsize = unsafe { std::mem::zeroed() };
                    unsafe { nix::libc::ioctl(std::io::stdin().as_fd().as_raw_fd(), TIOCGWINSZ, &mut ws) };
                    // let instruction = format!("\x1b[{};{}t", ws.ws_row, ws.ws_col);
                    // master.write_all(instruction.as_bytes()).await.unwrap();
                    // master.flush().await.unwrap();
                }
                Ok(n) = master.read(&mut master_buffer) => {
                    let msg = &master_buffer[..n];
                    if n > 0 {
                        stdout.write_all(msg).await.unwrap();
                        stdout.flush().await.unwrap();
                    } else {
                        dbg!("remote is closed");
                        break;
                    }
                }
            }
        }
    });

    dbg!("reset tty");
    tcsetattr(std::io::stdin().as_fd(), SetArg::TCSAFLUSH, &old_tty).unwrap();
    exit(0);
}
