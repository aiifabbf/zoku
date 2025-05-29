use std::{
    collections::VecDeque,
    ffi::CString,
    os::fd::{FromRawFd, IntoRawFd},
    path::Path,
};

use nix::{
    pty::{ForkptyResult, forkpty},
    unistd::execvp,
};
use tokio::{
    fs::{File, remove_file},
    io::{AsyncReadExt, AsyncWriteExt},
    net::UnixListener,
    runtime::Runtime,
    select, spawn,
    sync::mpsc::{channel, unbounded_channel},
};

const BUFFER_SIZE: usize = 4096; // bytes
const REPLAY_SIZE: usize = 1_000_000; // bytes

pub fn main(bind: &Path, argv: &[CString]) {
    match unsafe { forkpty(None, None).unwrap() } {
        ForkptyResult::Parent { child: _, master } => {
            let rt = Runtime::new().unwrap();
            rt.block_on(async {
                let (new_client_sender, mut new_client_receiver) = unbounded_channel();
                let (from_client_sender, mut from_client_receiver) = channel::<Vec<u8>>(BUFFER_SIZE);
                let listener = UnixListener::bind(bind).expect("address already in use");

                let to_master_sender = from_client_sender.clone();
                let _listener_worker = spawn(async move {
                    while let Ok((mut client, _addr)) = listener.accept().await {
                        let (from_master_sender, mut from_master_receiver) =
                            channel::<Vec<u8>>(BUFFER_SIZE);
                        // dbg!("sending channels to master");
                        new_client_sender.send(from_master_sender).unwrap();
                        let to_master_sender = to_master_sender.clone();
                        let _client_worker = spawn(async move {
                            loop {
                                let mut buffer = [0; BUFFER_SIZE];
                                select! {
                                    Ok(n) = client.read(&mut buffer) => {
                                        if n == 0 {
                                            break;
                                        }
                                        // dbg!("client worker: sending to master {}", from_utf8(&buffer));
                                        to_master_sender.send(buffer[..n].to_owned()).await.unwrap();
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

                let mut replay = VecDeque::new();
                let mut clients = vec![];
                let mut read =
                    unsafe { File::from_raw_fd(master.try_clone().unwrap().into_raw_fd()) };
                let mut write = unsafe { File::from_raw_fd(master.into_raw_fd()) };

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
                                to_new_client_sender.send(replay.as_slices().0.to_owned()).await.unwrap();
                                to_new_client_sender.send(replay.as_slices().1.to_owned()).await.unwrap();
                                clients.push(to_new_client_sender);
                                // dbg!("master worker: replay sent");
                            }
                        Ok(n) = read.read(&mut buffer) => {
                            if n == 0 {
                                break;
                            }
                            // dbg!("master worker: reading from process {}", from_utf8(&buffer));
                            // dbg!("master worker: extending replay with delta");
                            replay.extend(&buffer[..n]);
                            loop {
                                if replay.len() < REPLAY_SIZE {
                                    break;
                                } else if let Some(index) = replay.iter().position(|c| *c as char == '\n') {
                                    replay.drain(..index + 1);
                                } else {
                                    break;
                                }
                            }

                            let mut active_clients = vec![];

                            for to_client_sender in clients.into_iter() {
                                // dbg!("master worker: sending delta to client");
                                if to_client_sender.send(buffer[..n].to_owned()).await.is_ok() {
                                    active_clients.push(to_client_sender);
                                }
                            }
                            clients = active_clients;
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
