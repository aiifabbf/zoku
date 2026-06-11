use std::{collections::VecDeque, ffi::CString, iter::once, os::fd::AsRawFd};

use async_signal::{Signal, Signals};
use nix::{
    libc::{TIOCSWINSZ, ioctl},
    pty::{ForkptyResult, Winsize, forkpty},
    sys::wait::waitpid,
    unistd::execvp,
};
use smol::{
    Unblock, block_on,
    channel::{Receiver, Sender, bounded, unbounded},
    fs::remove_file,
    future::FutureExt,
    io::{AsyncReadExt, AsyncWriteExt, split},
    net::unix::{UnixListener, UnixStream},
    spawn,
    stream::StreamExt,
};

const BUFFER_SIZE: usize = 4096; // bytes
const CHANNEL_SIZE: usize = 1_000; // messages
const REPLAY_SIZE: usize = 10_000; // lines

enum Message {
    Raw(Vec<u8>),
    Resize(u16, u16),
}

enum Replay {
    Normal(VecDeque<Vec<u8>>),
    Alternate(VecDeque<Vec<u8>>, VecDeque<u8>),
}

impl Default for Replay {
    fn default() -> Self {
        Self::Normal(VecDeque::new())
    }
}

const EMPTY: &[u8] = b"";
const ENTER_ALTERNATE: &[u8] = b"\x1b[?1049h";
const LEAVE_ALTERNATE: &[u8] = b"\x1b[?1049l";
const QUERY_COMMANDS: &[&[u8]] = &[
    b"\x1b[6n",  // query cursor position
    b"\x1b[5n",  // query terminal status
    b"\x1b[c",   // request device code
    b"\x1b[0c",  // request device code
    b"\x1b[18t", // request terminal window size
    b"\x1b[13t", // request terminal window position
    b"\x1b[>q",  // report xterm name and version
    b"\x1b[>0q", // report xterm name and version
];
// There are so many commands that can cause terminal emulator to respond: https://invisible-island.net/xterm/ctlseqs/ctlseqs.html
// Use regex automata for a complete solution?

impl Replay {
    fn feed(self, bytes: &[u8]) -> Self {
        match bytes {
            b"" => self,
            [head, tail @ ..] => match self {
                Self::Normal(mut replay) => {
                    if let Some(last) = replay.back_mut() {
                        if let Some(b'\n') = last.last() {
                            replay.push_back(vec![*head]);
                        } else {
                            last.extend([head]);
                        }
                    } else {
                        replay.push_back(vec![*head]);
                    }

                    let len = replay.len();
                    if len > REPLAY_SIZE {
                        replay.drain(..len - REPLAY_SIZE);
                    }

                    if let Some(last) = replay.back_mut() {
                        if last.ends_with(ENTER_ALTERNATE) {
                            last.drain(last.len() - ENTER_ALTERNATE.len()..);
                            Self::Alternate(replay, ENTER_ALTERNATE.iter().cloned().collect())
                                .feed(tail)
                        } else if last.ends_with(LEAVE_ALTERNATE) {
                            last.drain(last.len() - LEAVE_ALTERNATE.len()..);
                            Self::Normal(replay).feed(tail)
                        } else {
                            for nop in QUERY_COMMANDS {
                                if last.ends_with(nop) {
                                    last.drain(last.len() - nop.len()..);
                                }
                            }
                            Self::Normal(replay).feed(tail)
                        }
                    } else {
                        Self::Normal(replay).feed(tail)
                    }
                }
                Self::Alternate(replay, mut latest) => {
                    latest.extend([head]);

                    // I do not know how to truncate alternate buffer. It would corrupt alternate buffer if done wrongly. So here I just do not replay anything on alternate buffer. Hope all well-written TUI apps would properly redraw whole screen upon window resizing!
                    let len = latest.len();
                    if len > ENTER_ALTERNATE.len() {
                        latest.drain(..len - ENTER_ALTERNATE.len());
                    }
                    debug_assert!(latest.len() <= ENTER_ALTERNATE.len());

                    if latest
                        .iter()
                        .rev()
                        .take(LEAVE_ALTERNATE.len())
                        .eq(LEAVE_ALTERNATE.iter().rev())
                    {
                        Self::Normal(replay).feed(tail)
                    } else if latest
                        .iter()
                        .rev()
                        .take(ENTER_ALTERNATE.len())
                        .eq(ENTER_ALTERNATE.iter().rev())
                    {
                        Self::Alternate(replay, latest).feed(tail)
                    } else {
                        Self::Alternate(replay, latest).feed(tail)
                    }
                }
            },
        }
    }

    fn replay(&self) -> impl Iterator<Item = &[u8]> {
        match self {
            Self::Normal(replay) => replay.iter().map(AsRef::as_ref).chain(once(EMPTY)),
            Self::Alternate(replay, _latest) => replay
                .iter()
                .map(AsRef::as_ref)
                .chain(once(ENTER_ALTERNATE)),
        }
    }
}

async fn handle_new_client(
    client: UnixStream,
    from_master: Receiver<Vec<u8>>,
    to_master: Sender<Message>,
) -> Option<()> {
    let (mut client_read, mut client_write) = split(client);
    let client_read_worker = async move {
        loop {
            let mut buffer = [0; BUFFER_SIZE];
            let mut length = [0; 2];
            if let Ok(_) = client_read.read_exact(&mut length).await {
                let len = i16::from_be_bytes(length);
                if len > 0 {
                    let len = len as usize;
                    client_read.read_exact(&mut buffer[..len]).await.ok()?;
                    let msg = &buffer[..len];
                    to_master.send(Message::Raw(msg.to_owned())).await.ok()?;
                } else {
                    let mut row = [0; 2];
                    let mut col = [0; 2];
                    client_read.read_exact(&mut row).await.ok()?;
                    client_read.read_exact(&mut col).await.ok()?;
                    let row = u16::from_be_bytes(row);
                    let col = u16::from_be_bytes(col);
                    to_master.send(Message::Resize(row, col)).await.ok()?;
                }
            } else {
                break;
            }
        }
        Some(())
    };
    let client_write_worker = async move {
        loop {
            if let Some(delta) = from_master.recv().await.ok() {
                // dbg!("client worker: writing to client {}", from_utf8(&delta));
                client_write.write_all(&delta).await.ok()?;
                client_write.flush().await.ok()?;
            } else {
                break;
            }
        }
        Some(())
    };
    client_write_worker.or(client_read_worker).await?;
    Some(())
}

pub fn main(listener: std::os::unix::net::UnixListener, argv: &[CString]) {
    let winsize = Winsize {
        ws_row: 24,
        ws_col: 80,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    let bind = listener
        .local_addr()
        .unwrap()
        .as_pathname()
        .unwrap()
        .to_path_buf();
    match unsafe { forkpty(&winsize, None).unwrap() } {
        ForkptyResult::Parent { child, master } => {
            block_on(async {
                let (new_client_sender, new_client_receiver) = unbounded();
                let (from_client_sender, from_client_receiver) = bounded::<Message>(CHANNEL_SIZE);
                let listener = UnixListener::try_from(listener).unwrap();

                let to_master_sender = from_client_sender.clone();
                let listener_worker = async move {
                    while let Ok((client, _addr)) = listener.accept().await {
                        let (from_master_sender, from_master_receiver) =
                            bounded::<Vec<u8>>(CHANNEL_SIZE);
                        // dbg!("sending channels to master");
                        new_client_sender.send(from_master_sender).await.ok()?;
                        let to_master_sender = to_master_sender.clone();
                        spawn(handle_new_client(
                            client,
                            from_master_receiver,
                            to_master_sender,
                        ))
                        .detach();
                    }
                    Some(())
                };

                let mut replay = Replay::default();
                // smol isolates blocking read on another thread. Async read from a file is effectively the same as recv-ing from a bounded channel. This bounded channel has an insanely large buffer size 8MB for TTY device files (which are not real disk files), which causes significant lag when you try to Ctrl+C during `yes`. I notice ~4s lag on macOS between pressing Ctrl+C and seeing `yes` stops.
                // https://docs.rs/smol/latest/smol/struct.Unblock.html#method.with_capacity
                let mut read = Unblock::with_capacity(
                    CHANNEL_SIZE,
                    std::fs::File::from(master.try_clone().unwrap()),
                );
                let mut write = Unblock::with_capacity(
                    CHANNEL_SIZE,
                    std::fs::File::from(master.try_clone().unwrap()),
                );
                let mut signals = Signals::new([Signal::Child]).unwrap();

                let master_worker = async move {
                    while let Some(msg) = from_client_receiver.recv().await.ok() {
                        match msg {
                            Message::Raw(bytes) => {
                                write.write_all(&bytes).await.ok()?;
                                write.flush().await.ok()?;
                            }
                            Message::Resize(row, col) => {
                                let winsize = Winsize {
                                    ws_row: row,
                                    ws_col: col.saturating_sub(1),
                                    ws_xpixel: 0,
                                    ws_ypixel: 0,
                                };
                                unsafe { ioctl(master.as_raw_fd(), TIOCSWINSZ, &winsize) };
                                let winsize = Winsize {
                                    ws_col: col,
                                    ..winsize
                                };
                                unsafe { ioctl(master.as_raw_fd(), TIOCSWINSZ, &winsize) };
                                // Why do it twice? To force redraw. Some TUI app such as vim does not redraw whole screen if new size is the same as old size.
                            }
                        }
                    }
                    Some(())
                };

                let main_worker = async move {
                    enum Event<'a> {
                        Incoming(Sender<Vec<u8>>),
                        Stdout(&'a [u8]),
                        Closed,
                    }

                    let mut clients = vec![];
                    let mut buffer = [0; BUFFER_SIZE];

                    loop {
                        let event = async {
                            if let Some(new_client) = new_client_receiver.recv().await.ok() {
                                Event::Incoming(new_client)
                            } else {
                                Event::Closed
                            }
                        }
                        .or(async {
                            if let Ok(n) = read.read(&mut buffer).await {
                                if n > 0 {
                                    Event::Stdout(&buffer[..n])
                                } else {
                                    Event::Closed
                                }
                            } else {
                                Event::Closed
                            }
                        })
                        .or(async {
                            signals.next().await;
                            Event::Closed
                        })
                        .await;

                        match event {
                            Event::Incoming(to_new_client) => {
                                for line in replay.replay() {
                                    if !to_new_client.send(line.to_owned()).await.is_ok() {
                                        break;
                                    }
                                }
                                clients.push(to_new_client);
                            }
                            Event::Stdout(delta) => {
                                replay = replay.feed(delta);
                                let mut active_clients = vec![];

                                for to_client_sender in clients.into_iter() {
                                    if to_client_sender.send(delta.to_owned()).await.is_ok() {
                                        active_clients.push(to_client_sender);
                                    }
                                }
                                clients = active_clients;
                            }
                            Event::Closed => {
                                waitpid(child, None).ok()?;
                                break;
                            }
                        }
                    }
                    Some(())
                };

                listener_worker.or(master_worker).or(main_worker).await;
                remove_file(bind).await.ok()?;
                Some(())
            });
        }
        #[expect(unreachable_code)]
        ForkptyResult::Child => {
            let sh = CString::new("/bin/sh".as_bytes()).unwrap();
            let path = argv.get(0).unwrap_or(&sh);
            execvp(&path, &argv).unwrap();
        }
    }
}
