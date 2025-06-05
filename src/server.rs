use std::{
    collections::VecDeque,
    ffi::CString,
    iter::once,
    os::fd::{AsRawFd, FromRawFd, IntoRawFd},
    path::Path,
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
    net::UnixListener,
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
static EMPTY_VEC_DEQUE: VecDeque<u8> = VecDeque::new();
const ENTER_ALTERNATE: &[u8] = b"\x1b[?1049h";
const LEAVE_ALTERNATE: &[u8] = b"\x1b[?1049l";
const CLEAR: &[u8] = b"\x1b[2J";

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
                            Self::Normal(replay).feed(tail)
                        }
                    } else {
                        Self::Normal(replay).feed(tail)
                    }
                }
                Self::Alternate(replay, mut latest) => {
                    latest.extend([head]);

                    // Do not know how to truncate alternate buffer. Would corrupt alternate buffer if done wrongly.

                    // let len = latest.len();
                    // if len > ENTER_ALTERNATE.len() {
                    //     latest.drain(..len - ENTER_ALTERNATE.len());
                    // }

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
                        latest.drain(latest.len() - ENTER_ALTERNATE.len()..);
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
            Self::Normal(replay) => replay
                .iter()
                .map(AsRef::as_ref)
                .chain(once(EMPTY))
                .chain(once(EMPTY_VEC_DEQUE.as_slices().0))
                .chain(once(EMPTY_VEC_DEQUE.as_slices().1)),
            Self::Alternate(replay, latest) => replay
                .iter()
                .map(AsRef::as_ref)
                .chain(once(ENTER_ALTERNATE))
                .chain(once(latest.as_slices().0))
                .chain(once(latest.as_slices().1)),
        }
    }

    fn usage(&self) -> (usize, usize) {
        match self {
            Self::Normal(replay) => (replay.len(), 0),
            Self::Alternate(replay, latest) => (replay.len(), latest.len()),
        }
    }
}

pub fn main(bind: &Path, argv: &[CString]) {
    let winsize = Winsize {
        ws_row: 24,
        ws_col: 80,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    match unsafe { forkpty(&winsize, None).unwrap() } {
        ForkptyResult::Parent { child, master } => {
            let rt = Builder::new_current_thread().enable_all().build().unwrap();
            rt.block_on(async {
                let (new_client_sender, mut new_client_receiver) = unbounded_channel();
                let (from_client_sender, mut from_client_receiver) =
                    channel::<Message>(CHANNEL_SIZE);
                let listener = UnixListener::bind(bind).expect("address already in use");

                let to_master_sender = from_client_sender.clone();
                let _listener_worker = spawn(async move {
                    while let Ok((mut client, _addr)) = listener.accept().await {
                        let (from_master_sender, mut from_master_receiver) =
                            channel::<Vec<u8>>(CHANNEL_SIZE);
                        // dbg!("sending channels to master");
                        new_client_sender.send(from_master_sender).unwrap();
                        let to_master_sender = to_master_sender.clone();
                        let _client_worker = spawn(async move {
                            loop {
                                let mut buffer = [0; BUFFER_SIZE];
                                let mut length = [0; 2];
                                select! {
                                    biased;
                                    Ok(n) = client.read_exact(&mut length) => {
                                        if n == 0 {
                                            break;
                                        }
                                        let len = i16::from_be_bytes(length);
                                        if len > 0 {
                                            let len = len as usize;
                                            client.read_exact(&mut buffer[..len]).await.ok()?;
                                            let msg = &buffer[..len];
                                            // dbg!("client worker: sending to master {}", from_utf8(&buffer));
                                            to_master_sender.send(Message::Raw(msg.to_owned())).await.ok()?;
                                        } else {
                                            let mut row = [0; 2];
                                            let mut col = [0; 2];
                                            client.read_exact(&mut row).await.ok()?;
                                            client.read_exact(&mut col).await.ok()?;
                                            let row = u16::from_be_bytes(row);
                                            let col = u16::from_be_bytes(col);
                                            to_master_sender.send(Message::Resize(row, col)).await.ok()?;
                                        }
                                    }
                                    Some(delta) = from_master_receiver.recv() => {
                                        // dbg!("client worker: writing to client {}", from_utf8(&delta));
                                        client.write_all(&delta).await.ok()?;
                                        client.flush().await.ok()?;
                                    }
                                    else => break
                                };
                            }
                            Some(())
                        });
                    }
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
                            },
                            Message::Resize(row, col) => {
                                let winsize = Winsize {
                                    ws_row: row,
                                    ws_col: col,
                                    ws_xpixel: 0,
                                    ws_ypixel: 0,
                                };
                                dbg!("master worker: resize to", winsize);
                                unsafe { ioctl(write.as_raw_fd(), TIOCSWINSZ, &winsize) };
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
                                    to_new_client_sender.send(line.to_owned()).await.unwrap();
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
                            waitpid(child, None).unwrap();
                            // dbg!("master worker: child process exits");
                            break;
                        }
                        else => break
                    }
                }

                remove_file(bind).await.unwrap();
            })
        }
        ForkptyResult::Child => {
            let sh = CString::new("/bin/sh".as_bytes()).unwrap();
            let path = argv.get(0).unwrap_or(&sh);
            execvp(&path, &argv).unwrap();
        }
    }
}
