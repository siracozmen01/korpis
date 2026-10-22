// SPDX-License-Identifier: AGPL-3.0-or-later

//! A web program for the end-to-end tests, built static and run in a container. On port 8080 it
//! answers every HTTP request with `served` and the request's head as it arrived, so a test sees
//! what a proxy added on the way, save a WebSocket upgrade, which it accepts and then sends back
//! every byte; on port 8443 it sends back every byte it is sent, so a test sees that what it sent
//! arrived unchanged, as a TLS stream passed through by name does.

use std::io::{BufRead as _, BufReader, Read, Write as _};
use std::net::{TcpListener, TcpStream};
use std::thread::JoinHandle;

/// The longest request head it reads.
const MAX_HEAD: u64 = 64 * 1024;

/// The request head on `stream`, up to the blank line ending it.
fn head(stream: &TcpStream) -> std::io::Result<String> {
    let mut reader = BufReader::new(Read::take(stream, MAX_HEAD));
    let mut head = String::new();
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 || line.trim_end().is_empty() {
            return Ok(head);
        }
        head.push_str(&line);
    }
}

/// Answers one request with what it was, or a WebSocket upgrade by switching and echoing.
fn answer(stream: TcpStream) -> std::io::Result<()> {
    let mut stream = stream;
    let head = head(&stream)?;
    if head.to_ascii_lowercase().contains("\nupgrade: websocket") {
        stream.write_all(
            b"HTTP/1.1 101 Switching Protocols\r\nupgrade: websocket\r\nconnection: upgrade\r\n\r\n",
        )?;
        return echo(stream);
    }
    let body = format!("served\n{head}");
    let length = body.len();
    write!(
        stream,
        "HTTP/1.1 200 OK\r\ncontent-type: text/plain\r\ncontent-length: {length}\r\n\
         connection: close\r\n\r\n{body}"
    )?;
    stream.flush()
}

/// Sends back what it is sent until the sender stops.
fn echo(stream: TcpStream) -> std::io::Result<()> {
    let mut stream = stream;
    let mut back = stream.try_clone()?;
    std::io::copy(&mut stream, &mut back)?;
    Ok(())
}

/// Serves `each` connection to `port` on a thread of its own.
fn serve(port: u16, each: fn(TcpStream) -> std::io::Result<()>) -> std::io::Result<JoinHandle<()>> {
    let listener = TcpListener::bind(("0.0.0.0", port))?;
    Ok(std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            std::thread::spawn(move || {
                let _ = each(stream);
            });
        }
    }))
}

fn main() -> std::io::Result<()> {
    let served = [serve(8080, answer)?, serve(8443, echo)?];
    for s in served {
        let _ = s.join();
    }
    Ok(())
}
