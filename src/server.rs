use std::{
    collections::VecDeque,
    ffi::CString,
    os::fd::{FromRawFd, IntoRawFd},
    path::Path,
};

use nix::{
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
                    channel::<Vec<u8>>(CHANNEL_SIZE);
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
                                select! {
                                    biased;
                                    Ok(n) = client.read(&mut buffer) => {
                                        if n == 0 {
                                            break;
                                        }
                                        let msg = &buffer[..n];
                                        // dbg!("client worker: sending to master {}", from_utf8(&buffer));
                                        to_master_sender.send(msg.to_owned()).await.unwrap();
                                    }
                                    Some(delta) = from_master_receiver.recv() => {
                                        // dbg!("client worker: writing to client {}", from_utf8(&delta));
                                        client.write_all(&delta).await.unwrap();
                                        client.flush().await.unwrap();
                                    }
                                    else => break
                                };
                            }
                        });
                    }
                });

                let mut replay = VecDeque::<Vec<u8>>::new();
                let mut clients = vec![];
                let mut read =
                    unsafe { File::from_raw_fd(master.try_clone().unwrap().into_raw_fd()) };
                let mut write = unsafe { File::from_raw_fd(master.into_raw_fd()) };
                let mut signals = signal(SignalKind::child()).unwrap();

                let _master_worker = spawn(async move {
                    while let Some(delta) = from_client_receiver.recv().await {
                        // dbg!("master worker: writing to process {}", from_utf8(&delta));
                        write.write_all(&delta).await.ok()?;
                        write.flush().await.ok()?;
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
                                for line in replay.iter() {
                                    to_new_client_sender.send(line.clone()).await.unwrap();
                                }
                                clients.push(to_new_client_sender);
                                // dbg!("master worker: replay sent");
                            }
                        Ok(n) = read.read(&mut buffer) => {
                            if n == 0 {
                                break;
                            }
                            let msg = &buffer[..n];
                            // dbg!("master worker: reading from process {}", from_utf8(&buffer));
                            // dbg!("master worker: extending replay with delta");

                            for line in msg.split_inclusive(|c| *c == b'\n') {
                                if let Some(last) = replay.back_mut() {
                                    if let Some(b'\n') = last.last() {
                                        replay.push_back(line.to_owned());
                                    } else {
                                        last.extend(line);
                                    }
                                } else {
                                    replay.push_back(line.to_owned());
                                }
                            }
                            let len = replay.len();
                            if len > REPLAY_SIZE {
                                replay.drain(..len - REPLAY_SIZE);
                            }
                            // dbg!("master worker: keep latest replay", replay.len());

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
