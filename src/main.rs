use std::{
    env::{args, args_os},
    ffi::CString,
    os::unix::ffi::OsStrExt,
    path::Path,
};

mod client;
mod server;

fn main() {
    match [args().nth(1).as_deref(), args().nth(2).as_deref()] {
        [Some("new"), Some(path)] => {
            let argv: Vec<_> = args_os()
                .skip(3)
                .map(|arg| CString::new(arg.as_bytes()).unwrap())
                .collect();
            server::main(Path::new(path), &argv);
        }
        [Some("attach"), Some(path)] => client::main(Path::new(path)),
        _ => {
            println!(
                "Usage:
    zoku new path program
    zoku attach path"
            );
        }
    }
}
