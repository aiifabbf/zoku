use std::{
    collections::VecDeque,
    ffi::CString,
    iter::once,
    os::fd::{AsRawFd, FromRawFd, IntoRawFd},
};

use nix::{
    libc::{TIOCSWINSZ, ioctl},
    pty::{ForkptyResult, Winsize, forkpty},
    sys::wait::waitpid,
    unistd::execvp,
};
use tokio::{
    fs::{File, remove_file},
    io::{AsyncReadExt, AsyncWriteExt},
    runtime::Builder,
    select,
    signal::unix::{SignalKind, signal},
    spawn,
    sync::mpsc::{channel, unbounded_channel},
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
            let rt = Builder::new_current_thread().enable_all().build().unwrap();
            rt.block_on(async {
                let (new_client_sender, mut new_client_receiver) = unbounded_channel();
                let (from_client_sender, mut from_client_receiver) =
                    channel::<Message>(CHANNEL_SIZE);
                listener.set_nonblocking(true).unwrap();
                let listener = tokio::net::UnixListener::from_std(listener).unwrap();

                let to_master_sender = from_client_sender.clone();
                let _listener_worker = spawn(async move {
                    while let Ok((client, _addr)) = listener.accept().await {
                        let (from_master_sender, mut from_master_receiver) =
                            channel::<Vec<u8>>(CHANNEL_SIZE);
                        // dbg!("sending channels to master");
                        new_client_sender.send(from_master_sender).ok()?;
                        let to_master_sender = to_master_sender.clone();
                        let (mut client_read, mut client_write) = client.into_split();
                        let _client_read_worker = spawn(async move {
                            loop {
                                let mut buffer = [0; BUFFER_SIZE];
                                let mut length = [0; 2];
                                if let Ok(2) = client_read.read_exact(&mut length).await {
                                    let len = i16::from_be_bytes(length);
                                    if len > 0 {
                                        let len = len as usize;
                                        client_read.read_exact(&mut buffer[..len]).await.ok()?;
                                        let msg = &buffer[..len];
                                        // dbg!("client worker: sending to master {}", from_utf8(&buffer));
                                        to_master_sender
                                            .send(Message::Raw(msg.to_owned()))
                                            .await
                                            .ok()?;
                                    } else {
                                        let mut row = [0; 2];
                                        let mut col = [0; 2];
                                        client_read.read_exact(&mut row).await.ok()?;
                                        client_read.read_exact(&mut col).await.ok()?;
                                        let row = u16::from_be_bytes(row);
                                        let col = u16::from_be_bytes(col);
                                        to_master_sender
                                            .send(Message::Resize(row, col))
                                            .await
                                            .ok()?;
                                    }
                                } else {
                                    break;
                                }
                            }
                            Some(())
                        });
                        let _client_write_worker = spawn(async move {
                            loop {
                                if let Some(delta) = from_master_receiver.recv().await {
                                    // dbg!("client worker: writing to client {}", from_utf8(&delta));
                                    client_write.write_all(&delta).await.ok()?;
                                    client_write.flush().await.ok()?;
                                } else {
                                    break;
                                }
                            }
                            Some(())
                        });
                    }
                    Some(())
                });

                let mut replay = Replay::default();
                let mut clients = vec![];
                let mut read =
                    unsafe { File::from_raw_fd(master.try_clone().unwrap().into_raw_fd()) };
                let mut write = unsafe { File::from_raw_fd(master.into_raw_fd()) };
                let mut signals = signal(SignalKind::child()).unwrap();

                let _master_worker = spawn(async move {
                    while let Some(msg) = from_client_receiver.recv().await {
                        match msg {
                            Message::Raw(bytes) => {
                                // dbg!("master worker: writing to process {}", std::str::from_utf8(&bytes));
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
                                unsafe { ioctl(write.as_raw_fd(), TIOCSWINSZ, &winsize) };
                                // dbg!("master worker: resize to", winsize);
                                let winsize = Winsize {
                                    ws_col: col,
                                    ..winsize
                                };
                                unsafe { ioctl(write.as_raw_fd(), TIOCSWINSZ, &winsize) };
                                // Why do it twice? To force redraw. Some TUI app such as vim does not redraw whole screen if new size is the same as old size.
                            }
                        }
                    }
                    Some(())
                });

                loop {
                    let mut buffer = [0; BUFFER_SIZE];
                    select! {
                        biased;
                        Some(to_new_client_sender) =
                            new_client_receiver.recv() => {
                                // dbg!("master worker: new client");
                                // dbg!("master worker: sending replay to client");
                                for line in replay.replay() {
                                    if !to_new_client_sender.send(line.to_owned()).await.is_ok() {
                                        break;
                                    }
                                }
                                clients.push(to_new_client_sender);
                                // dbg!("master worker: replay sent");
                            }
                        Ok(n) = read.read(&mut buffer) => {
                            if n == 0 {
                                break;
                            }
                            let msg = &buffer[..n];
                            // dbg!("master worker: reading from process {}", std::str::from_utf8(msg));

                            // dbg!("master worker: extending replay with delta");
                            replay = replay.feed(msg);
                            // dbg!("master worker: replay is at", match replay {
                            //     Replay::Normal(_) => "normal",
                            //     Replay::Alternate(_, _) => "alternate",
                            // });
                            // dbg!("master worker: keep latest replay", replay.len());
                            // dbg!("master worker: replay usage", replay.usage());

                            let mut active_clients = vec![];

                            for to_client_sender in clients.into_iter() {
                                // dbg!("master worker: sending delta to client");
                                if to_client_sender.send(msg.to_owned()).await.is_ok() {
                                    active_clients.push(to_client_sender);
                                }
                            }
                            clients = active_clients;
                        }
                        _ = signals.recv() => {
                            waitpid(child, None).ok()?;
                            // dbg!("master worker: child process exits");
                            break;
                        }
                        else => break
                    }
                }

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
