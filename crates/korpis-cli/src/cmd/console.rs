// SPDX-License-Identifier: AGPL-3.0-or-later

//! `korpis console` (§8 of docs/13-surface-cli.md): attach to a workload's console. Two data-path
//! sessions at a relay: one reading the stream from its scrollback and following it, and, where
//! the caller may type, one writing input (§6 of docs/14-streams.md). Reading and writing are
//! separate actions, so a caller who may only watch is attached read-only and told so.
//!
//! On a terminal the command puts it in raw mode, so every key reaches the workload as typed,
//! sends the terminal's size and every change of it, and detaches on `Ctrl-]`, as telnet does.
//! Output is written as the workload wrote it, its line ends included; what someone typed is
//! shown as a notice naming who typed it, as §6 of docs/14-streams.md shows it. With input from
//! a pipe, the command sends what the pipe holds, shows two more seconds of output, and ends.

use super::session::{Minted, Session, frame, mint, open, relay, renew};
use crate::connect::Connection;
use crate::error::CliError;
use crate::resolve;
use crate::terminal::{self, Raw};
use buffa::Message as _;
use clap::Args;
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::console_input::Input;
use korpis_proto::korpis::v1::__buffa::oneof::relay_response::Message as Reply;
use korpis_proto::korpis::v1::__buffa::oneof::session_frame::Frame;
use std::io::{Read as _, Write as _};
use std::time::Duration;
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::mpsc;

const READ: &str = "workload.console.read";
const WRITE: &str = "workload.console.write";
/// `Ctrl-]`: detach.
const DETACH: u8 = 0x1d;
/// How long output is still shown after a piped input ends.
const AFTER_PIPE: Duration = Duration::from_secs(2);

/// `korpis console`.
#[derive(Debug, Clone, Args)]
pub struct Console {
    /// The workload: an identifier, `project/name`, or a name.
    pub workload: String,
    /// How many of the last records held to show before following.
    #[arg(long, default_value_t = 100)]
    pub scrollback: u64,
    /// Attach without typing, even where the token may type.
    #[arg(long)]
    pub read_only: bool,
    /// Open the sessions at this relay rather than the first the control plane names.
    #[arg(long)]
    pub relay: Option<String>,
}

/// One session and its token.
struct Open {
    session: Session,
    minted: Minted,
}

/// What a session sent.
enum Heard {
    Data(Vec<u8>),
    End(String, String),
    Lost(String),
    Nothing,
}

async fn hear(session: &mut Session) -> Heard {
    match session.message::<pb::RelayResponse>().await {
        Ok(Some(m)) => match m.to_owned_message().message {
            Some(Reply::Frame(f)) => match f.frame {
                Some(Frame::Data(bytes)) => Heard::Data(bytes),
                Some(Frame::End(e)) => Heard::End(e.code, e.message),
                _ => Heard::Nothing,
            },
            _ => Heard::Nothing,
        },
        Ok(None) => Heard::Lost("the relay closed the session".into()),
        Err(e) => Heard::Lost(CliError::from(e).to_string()),
    }
}

/// What `w` sent, or never, if there is no `w`.
async fn hear_maybe(w: &mut Option<Open>) -> Heard {
    match w {
        Some(o) => hear(&mut o.session).await,
        None => std::future::pending().await,
    }
}

/// The local side: raw mode on a terminal, keys read on a thread of their own.
struct Local {
    raw: Option<Raw>,
    keys: mpsc::Receiver<Vec<u8>>,
}

fn local(typing: bool) -> Local {
    let raw = if typing { Raw::enter() } else { None };
    let (tx, keys) = mpsc::channel(64);
    if typing {
        std::thread::spawn(move || read_keys(&tx));
    }
    Local { raw, keys }
}

/// Reads the local input until it ends, or until nothing takes it.
fn read_keys(tx: &mpsc::Sender<Vec<u8>>) {
    let mut buf = [0u8; 4096];
    while let Ok(n @ 1..) = std::io::stdin().read(&mut buf)
        && tx.blocking_send(buf.get(..n).unwrap_or_default().to_vec()).is_ok()
    {}
}

fn out(bytes: &[u8]) {
    let mut o = std::io::stdout().lock();
    let _ = o.write_all(bytes).and_then(|()| o.flush());
}

/// A notice for the person, on its own line whatever mode the terminal is in.
fn notice(raw: bool, text: &str) {
    let end = if raw { "\r\n" } else { "\n" };
    out(format!("{end}\u{2500}\u{2500} {text} \u{2500}\u{2500}{end}").as_bytes());
}

/// Who typed an input record and what, its line end left off (§6 of docs/14-streams.md): the
/// stream writer records each input as `{"by", "session", "text"}`. `None` for any other record.
fn typed(r: &pb::StreamRecord) -> Option<(String, String)> {
    if r.source != "input" {
        return None;
    }
    let v: serde_json::Value = serde_json::from_slice(&r.body).ok()?;
    let text = v.get("text")?.as_str()?;
    let text = text.strip_suffix('\n').unwrap_or(text);
    let text = text.strip_suffix('\r').unwrap_or(text);
    Some((v.get("by")?.as_str()?.to_owned(), text.to_owned()))
}

/// Writes what a chunk holds; the offset to read from next.
fn show(bytes: &[u8], raw: bool) -> Result<u64, CliError> {
    let chunk = pb::ConsoleChunk::decode_from_slice(bytes)
        .map_err(|e| CliError::Transport(format!("a console chunk: {e}")))?;
    if let Some(m) = chunk.missing.as_option() {
        let last = m.end.saturating_sub(1);
        notice(raw, &format!("offsets {} to {last} are no longer held on the node", m.first));
    }
    for r in &chunk.records {
        match (r.gap.as_option(), typed(r)) {
            (Some(g), _) => notice(raw, &format!("{} records dropped: {}", g.records, g.reason)),
            (None, Some((by, text))) => notice(raw, &format!("input {text:?} by {by}")),
            (None, None) => out(&r.body),
        }
    }
    Ok(chunk.next_offset)
}

async fn open_read(
    at: &Connection,
    c: &Connection,
    w: &str,
    read: &pb::ConsoleRead,
) -> Result<Open, CliError> {
    let minted = mint(c, w, READ).await?;
    let session = open(at, w, &minted, read.encode_to_vec()).await?;
    Ok(Open { session, minted })
}

/// The write session, if the caller may type; why not, if not.
async fn open_write(at: &Connection, c: &Connection, w: &str) -> Result<Open, String> {
    let minted = mint(c, w, WRITE).await.map_err(|e| e.to_string())?;
    let session = open(at, w, &minted, Vec::new()).await.map_err(|e| e.to_string())?;
    Ok(Open { session, minted })
}

async fn send(w: &mut Option<Open>, input: Input) -> Result<(), CliError> {
    let Some(o) = w else { return Ok(()) };
    let bytes = pb::ConsoleInput { input: Some(input), ..Default::default() }.encode_to_vec();
    o.session.send(frame(Frame::Data(bytes))).await?;
    Ok(())
}

fn resize_input() -> Option<Input> {
    let (cols, rows) = terminal::size()?;
    let size =
        pb::TerminalSize { cols: u32::from(cols), rows: u32::from(rows), ..Default::default() };
    Some(Input::Resize(Box::new(size)))
}

/// The attachment: who it is for, and where.
struct Attached<'a> {
    c: &'a Connection,
    at: Connection,
    workload: String,
    raw: bool,
}

impl Attached<'_> {
    /// Takes keys typed; whether to stay attached.
    async fn keys(
        &self,
        w: &mut Option<Open>,
        keys: Option<Vec<u8>>,
        ends: &mut Option<tokio::time::Instant>,
    ) -> Result<bool, CliError> {
        let Some(keys) = keys else {
            // The pipe ended: the input is closed, so the node ends its session once it has taken
            // all of it, and the output is shown a little longer before the command ends.
            if let Some(o) = w.as_mut() {
                o.session.close_send();
            }
            *ends = Some(tokio::time::Instant::now() + AFTER_PIPE);
            return Ok(true);
        };
        let (before, detach) = match keys.iter().position(|b| *b == DETACH && self.raw) {
            Some(i) => (keys.get(..i).unwrap_or_default().to_vec(), true),
            None => (keys, false),
        };
        if !before.is_empty() {
            send(w, Input::Data(before)).await?;
        }
        Ok(!detach)
    }

    /// A read session lost, opened again from where it was.
    async fn reopen(&self, next: u64) -> Result<Open, CliError> {
        let read = pb::ConsoleRead { from_offset: next, follow: true, ..Default::default() };
        let mut backoff = Duration::from_secs(1);
        loop {
            match open_read(&self.at, self.c, &self.workload, &read).await {
                Ok(o) => return Ok(o),
                Err(e) => notice(self.raw, &format!("reconnecting: {e}")),
            }
            tokio::time::sleep(backoff).await;
            backoff = (backoff * 2).min(Duration::from_secs(30));
        }
    }
}

/// Attaches to the workload's console as `a` asks, until detached.
pub async fn console(c: &Connection, a: &Console) -> Result<(), CliError> {
    let workload = resolve::workload(c, &a.workload).await?;
    let at = relay(c, a.relay.as_deref()).await?;
    let first = pb::ConsoleRead { last: a.scrollback, follow: true, ..Default::default() };
    let mut r = open_read(&at, c, &workload, &first).await?;
    let mut w = if a.read_only {
        None
    } else {
        match open_write(&at, c, &workload).await {
            Ok(o) => Some(o),
            Err(why) => {
                notice(false, &format!("read-only: {why}"));
                None
            }
        }
    };
    let mut local = local(w.is_some());
    let raw = local.raw.is_some();
    let me = Attached { c, at, workload, raw };
    if raw {
        if let Some(size) = resize_input() {
            send(&mut w, size).await?;
        }
        notice(true, "attached; Ctrl-] detaches");
    }
    let result = attached(&me, (&mut r, &mut w), &mut local).await;
    drop(local);
    if raw {
        notice(false, "detached");
    }
    result
}

async fn attached(
    me: &Attached<'_>,
    (r, w): (&mut Open, &mut Option<Open>),
    local: &mut Local,
) -> Result<(), CliError> {
    let mut winch =
        signal(SignalKind::window_change()).map_err(|e| CliError::Local(e.to_string()))?;
    let mut next = 0;
    let mut ends: Option<tokio::time::Instant> = None;
    loop {
        let write_renew = w.as_ref().map_or(Duration::MAX, |o| o.minted.renew_in());
        let ending = ends.unwrap_or_else(tokio::time::Instant::now);
        tokio::select! {
            heard = hear(&mut r.session) => me.on_read(heard, r, &mut next).await?,
            heard = hear_maybe(w) => {
                if ends.is_some() {
                    // The input was closed; its session ending is what was asked for.
                    *w = None;
                } else {
                    me.on_write(heard, w).await;
                }
            }
            keys = local.keys.recv(), if w.is_some() && ends.is_none() => {
                if !me.keys(w, keys, &mut ends).await? {
                    return Ok(());
                }
            }
            _ = winch.recv(), if me.raw => {
                if let Some(size) = resize_input() {
                    send(w, size).await?;
                }
            }
            () = tokio::time::sleep(r.minted.renew_in()) => {
                renew(me.c, &me.workload, &mut r.session, &mut r.minted).await?;
            }
            () = tokio::time::sleep(write_renew), if w.is_some() => {
                if let Some(o) = w.as_mut() {
                    renew(me.c, &me.workload, &mut o.session, &mut o.minted).await?;
                }
            }
            () = tokio::time::sleep_until(ending), if ends.is_some() => return Ok(()),
        }
    }
}

/// Whether a session that ended with `code` is opened again rather than given up.
fn passing(code: &str) -> bool {
    matches!(code, "" | "expired" | "unavailable")
}

impl Attached<'_> {
    /// What the read session sent.
    async fn on_read(&self, heard: Heard, r: &mut Open, next: &mut u64) -> Result<(), CliError> {
        match heard {
            Heard::Data(bytes) => *next = show(&bytes, self.raw)?,
            Heard::End(code, _) if passing(&code) => *r = self.reopen(*next).await?,
            Heard::End(code, message) => {
                return Err(CliError::Local(format!("the console ended: {code}: {message}")));
            }
            Heard::Lost(why) => {
                notice(self.raw, &format!("reconnecting: {why}"));
                *r = self.reopen(*next).await?;
            }
            Heard::Nothing => {}
        }
        Ok(())
    }

    /// What the write session sent: it sends nothing but its end. One lost is opened again;
    /// one refused leaves the attachment read-only, said so.
    async fn on_write(&self, heard: Heard, w: &mut Option<Open>) {
        let why = match heard {
            Heard::End(code, message) if !passing(&code) => format!("{code}: {message}"),
            Heard::End(..) | Heard::Lost(_) => {
                match open_write(&self.at, self.c, &self.workload).await {
                    Ok(o) => {
                        *w = Some(o);
                        return;
                    }
                    Err(why) => why,
                }
            }
            Heard::Data(_) | Heard::Nothing => return,
        };
        notice(self.raw, &format!("input ended, {why}; now read-only"));
        *w = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(source: &str, body: &str) -> pb::StreamRecord {
        pb::StreamRecord {
            source: source.to_owned(),
            body: body.as_bytes().to_vec(),
            ..Default::default()
        }
    }

    #[test]
    fn what_someone_typed_is_told_with_who_typed_it_and_nothing_else_is() {
        let typed_stop = record("input", r#"{"by":"user:alex","session":"s1","text":"stop\r\n"}"#);
        assert_eq!(typed(&typed_stop), Some(("user:alex".to_owned(), "stop".to_owned())));
        assert_eq!(typed(&record("stdout", r#"{"by":"x","text":"y"}"#)), None, "written");
        assert_eq!(typed(&record("input", "not json")), None, "shown as it is");
    }
}
