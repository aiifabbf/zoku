use std::collections::HashSet;
use std::ffi::CString;
use std::fs::remove_file;
use std::os::fd::{AsFd, AsRawFd, OwnedFd};
use std::os::unix::net::UnixListener;
use std::path::Path;
use std::str::from_utf8;
use std::sync::{Arc, Mutex};
use std::thread::spawn;

use nix::poll::{PollFd, PollFlags, PollTimeout, poll};
use nix::pty::{ForkptyResult, forkpty};
use nix::unistd::{execvp, read, write};

pub fn main(bind: &Path, argv: &[CString]) {
    match unsafe { forkpty(None, None).unwrap() } {
        ForkptyResult::Parent { child: _, master } => {
            let replay = Arc::new(Mutex::new(vec![]));
            let clients = Arc::new(Mutex::new(vec![]));
            let listener = UnixListener::bind(bind).expect("address already in use");
            {
                let clients = Arc::clone(&clients);
                let replay = Arc::clone(&replay);
                let new_client_worker = spawn(move || {
                    for client in listener.incoming() {
                        let client: OwnedFd = client.unwrap().into();
                        dbg!("waiting for clients write lock");
                        let mut clients = clients.lock().unwrap();
                        dbg!("waiting for replay read lock");
                        let replay = replay.lock().unwrap();
                        write(client.as_fd(), &replay[..]).unwrap();
                        clients.push(client);
                    }
                });
            }

            loop {
                dbg!("waiting for clients read lock");
                let mut clients = clients.lock().unwrap();
                dbg!("waiting for replay write lock");
                let mut replay = replay.lock().unwrap();

                let mut fds: Vec<_> = [PollFd::new(master.as_fd(), PollFlags::POLLIN)]
                    .into_iter()
                    .chain(
                        clients
                            .iter()
                            .map(|fd| PollFd::new(fd.as_fd(), PollFlags::POLLIN)),
                    )
                    .collect();

                let _ = poll(&mut fds, PollTimeout::from(1000u16)).unwrap();

                if fds[0].any().unwrap() {
                    let mut buffer = [0; 1024];
                    let n = read(master.as_fd(), &mut buffer).unwrap();
                    if n > 0 {
                        dbg!("received from master: ", from_utf8(&buffer[..n]));
                        replay.extend(&buffer[..n]);

                        for client in clients.iter() {
                            write(client.as_fd(), &buffer[..n]).unwrap();
                        }
                    } else {
                        dbg!("slave is closed");
                        break;
                    }
                }

                let mut inactive_fds = HashSet::new();

                for fd in fds.iter().skip(1) {
                    if fd.any().unwrap() {
                        let mut buffer = [0; 1024];
                        let n = read(fd.as_fd(), &mut buffer).unwrap();
                        if n > 0 {
                            dbg!("sending to master: ", from_utf8(&buffer[..n]));
                            write(master.as_fd(), &buffer[..n]).unwrap();
                        } else {
                            dbg!("client is closed");
                            inactive_fds.insert(fd.as_fd().as_raw_fd());
                        }
                    }
                }

                clients.retain(|fd| !inactive_fds.contains(&fd.as_fd().as_raw_fd()));
            }

            remove_file(bind).unwrap();
        }
        ForkptyResult::Child => {
            let sh = CString::new("/bin/sh".as_bytes()).unwrap();
            let path = argv.get(0).unwrap_or(&sh);
            dbg!("execvp {} {}", argv);
            execvp(&path, &argv).unwrap();
        }
    }
}
