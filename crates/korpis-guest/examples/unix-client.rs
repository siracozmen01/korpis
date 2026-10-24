// SPDX-License-Identifier: AGPL-3.0-or-later

//! A client of a Unix socket, which the driver's tests carry in a guest's image to reach its
//! `/run/korpis` from inside, since busybox's `nc` speaks no Unix sockets: it connects to the
//! socket its first argument names, writes its second and a newline, and copies what it is
//! answered to its output until the socket closes.

use std::io::{Read as _, Write as _};
use std::os::unix::net::UnixStream;
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [path, said] = args.as_slice() else {
        let _ = writeln!(std::io::stderr(), "usage: unix-client SOCKET TEXT");
        return ExitCode::from(2);
    };
    match talk(path, said) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            let _ = writeln!(std::io::stderr(), "unix-client: {path}: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Says `said` on the socket at `path` and writes the answer out.
fn talk(path: &str, said: &str) -> std::io::Result<()> {
    let mut stream = UnixStream::connect(path)?;
    stream.write_all(format!("{said}\n").as_bytes())?;
    stream.shutdown(std::net::Shutdown::Write)?;
    let mut answer = Vec::new();
    stream.read_to_end(&mut answer)?;
    std::io::stdout().write_all(&answer)
}
