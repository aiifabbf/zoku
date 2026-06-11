use std::io::Write;
use std::os::fd::{AsFd, AsRawFd};

use async_signal::{Signal, Signals};
use nix::libc::TIOCGWINSZ;
use nix::pty::Winsize;
use nix::sys::termios::{LocalFlags, SetArg, tcgetattr, tcsetattr};
use smol::future::FutureExt;
use smol::io::{AsyncReadExt, AsyncWriteExt, split};
use smol::net::unix::UnixStream;
use smol::stream::StreamExt;
use smol::{Unblock, block_on};

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

pub fn main(master: std::os::unix::net::UnixStream) {
    let old_tty = tcgetattr(std::io::stdin().as_fd()).unwrap();
    let mut tty = old_tty.clone();
    tty.local_flags.set(LocalFlags::ECHO, false);
    tty.local_flags.set(LocalFlags::ICANON, false);
    tty.local_flags.set(LocalFlags::ISIG, false);
    // dbg!("set -echo -icanon -isig");
    std::io::stdout().flush().unwrap();
    tcsetattr(std::io::stdin().as_fd(), SetArg::TCSAFLUSH, &tty).unwrap();

    block_on(async {
        let master = UnixStream::try_from(master).unwrap();
        let (mut master_read, mut master_write) = split(master);
        notify_resize(&mut master_write).await?;
        let mut signals = Signals::new([Signal::Winch]).unwrap();
        let mut stdin = Unblock::new(std::io::stdin());
        let mut stdout = Unblock::new(std::io::stdout());

        let screen_to_remote = async move {
            enum Event<'a> {
                Stdin(&'a [u8]),
                Resize,
                Detach,
            }
            let mut buffer = [0; BUFFER_SIZE];

            loop {
                let event = async {
                    if let Ok(n) = stdin.read(&mut buffer).await {
                        if n > 0 {
                            Event::Stdin(&buffer[..n])
                        } else {
                            Event::Detach
                        }
                    } else {
                        Event::Detach
                    }
                }
                .or(async {
                    if let Some(_) = signals.next().await {
                        Event::Resize
                    } else {
                        Event::Detach
                    }
                })
                .await;

                match event {
                    Event::Stdin(msg) => {
                        master_write
                            .write_all(&(msg.len() as i16).to_be_bytes())
                            .await
                            .ok()?;
                        master_write.write_all(msg).await.ok()?;
                        master_write.flush().await.ok()?;
                    }
                    Event::Resize => {
                        notify_resize(&mut master_write).await?;
                    }
                    Event::Detach => {
                        break;
                    }
                }
            }
            Some(())
        };

        let remote_to_screen = async move {
            let mut buffer = [0; BUFFER_SIZE];

            loop {
                if let Ok(n) = master_read.read(&mut buffer).await {
                    if n > 0 {
                        let msg = &buffer[..n];
                        stdout.write_all(msg).await.ok()?;
                        stdout.flush().await.ok()?;
                    } else {
                        break;
                    }
                } else {
                    break;
                }
            }
            Some(())
        };

        screen_to_remote.or(remote_to_screen).await?;
        Some(())
    });

    // dbg!("reset tty");
    tcsetattr(std::io::stdin().as_fd(), SetArg::TCSAFLUSH, &old_tty).unwrap();
}
