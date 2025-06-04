use std::io::Write;
use std::os::fd::{AsFd, AsRawFd};
use std::path::Path;
use std::process::exit;

use nix::libc::TIOCGWINSZ;
use nix::pty::Winsize;
use nix::sys::termios::{LocalFlags, SetArg, tcgetattr, tcsetattr};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use tokio::runtime::Builder;
use tokio::select;
use tokio::signal::unix::{SignalKind, signal};

const BUFFER_SIZE: usize = 4096; // bytes

async fn notify_resize(mut writer: impl AsyncWriteExt + Unpin) -> Option<()> {
    let mut ws: Winsize = unsafe { std::mem::zeroed() };
    unsafe { nix::libc::ioctl(std::io::stdin().as_fd().as_raw_fd(), TIOCGWINSZ, &mut ws) };
    // let instruction = format!("\x1b[{};{}t", ws.ws_row, ws.ws_col);
    writer.write_all(&(-4 as i16).to_be_bytes()).await.ok()?;
    writer.write_all(&ws.ws_row.to_be_bytes()).await.ok()?;
    writer.write_all(&ws.ws_col.to_be_bytes()).await.ok()?;
    writer.flush().await.ok()?;
    Some(())
}

pub fn main(path: &Path) {
    let old_tty = tcgetattr(std::io::stdin().as_fd()).unwrap();
    let mut tty = old_tty.clone();
    tty.local_flags.set(LocalFlags::ECHO, false);
    tty.local_flags.set(LocalFlags::ICANON, false);
    tty.local_flags.set(LocalFlags::ISIG, false);
    dbg!("set -echo -icanon -isig");
    std::io::stdout().flush().unwrap();
    tcsetattr(std::io::stdin().as_fd(), SetArg::TCSAFLUSH, &tty).unwrap();

    let rt = Builder::new_current_thread().enable_all().build().unwrap();
    rt.block_on(async {
        let mut master = UnixStream::connect(path).await.expect("cannot find server");
        notify_resize(&mut master).await?;
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
                        master.write_all(&(msg.len() as i16).to_be_bytes()).await.ok()?;
                        master.write_all(msg).await.ok()?;
                        master.flush().await.ok()?;
                    } else {
                        dbg!("master is closed");
                        break;
                    }
                }
                Some(()) = signals.recv() => {
                    notify_resize(&mut master).await?;
                }
                Ok(n) = master.read(&mut master_buffer) => {
                    let msg = &master_buffer[..n];
                    if n > 0 {
                        stdout.write_all(msg).await.ok()?;
                        stdout.flush().await.ok()?;
                    } else {
                        dbg!("remote is closed");
                        break;
                    }
                }
            }
        }
        Some(())
    });

    dbg!("reset tty");
    tcsetattr(std::io::stdin().as_fd(), SetArg::TCSAFLUSH, &old_tty).unwrap();
    exit(0);
}
