// SPDX-License-Identifier: AGPL-3.0-or-later

//! Moving a file's bytes over a file session: a mebibyte per request, a few requests sent ahead
//! of their answers so a distant relay does not set the pace, and never the whole file in memory.
//! An upload continues from the size the node reports, a download from the size of the local
//! file, when the caller says the partial file is the same one; otherwise each starts over.

use super::remote::Remote;
use crate::error::CliError;
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::file_request::Request;
use korpis_proto::korpis::v1::__buffa::oneof::file_response::Response;
use std::path::Path;
use tokio::io::{AsyncReadExt as _, AsyncSeekExt as _, AsyncWriteExt as _};

/// The most bytes one request moves.
pub const CHUNK: usize = 1 << 20;
/// The same, as a file offset counts.
const CHUNK_U64: u64 = 1 << 20;

fn wide(n: usize) -> u64 {
    u64::try_from(n).unwrap_or(u64::MAX)
}
/// The most requests sent ahead of their answers.
const AHEAD: u64 = 4;

fn local(path: &Path, e: &std::io::Error) -> CliError {
    CliError::Local(format!("{}: {e}", path.display()))
}

fn written(r: Response) -> Result<u64, CliError> {
    match r {
        Response::Written(n) => Ok(n),
        other => Err(CliError::Transport(format!("a write answered {other:?}"))),
    }
}

/// Where an upload of `len` bytes to `path` starts: from the size there when `resume` and the
/// file there is smaller, otherwise from the start.
///
/// # Errors
/// The node refused the stat, or the session ended.
pub async fn upload_from(
    r: &mut Remote,
    path: &str,
    len: u64,
    resume: bool,
) -> Result<u64, CliError> {
    if !resume {
        return Ok(0);
    }
    let there = r.stat(path).await?;
    let file = there.filter(|e| e.kind.as_known() == Some(pb::FileKind::FILE_KIND_FILE));
    Ok(file.map_or(0, |e| e.size).min(len))
}

/// Writes the open local file `file`, named `from`, to `path` from byte `start`; how many bytes
/// were sent. From 0, it replaces what is there. The file is given open, so what is read is the
/// file that was checked, whatever its path names by then.
///
/// # Errors
/// The local file could not be read, the node refused a write, or the session ended.
pub async fn upload(
    r: &mut Remote,
    (mut file, from): (tokio::fs::File, &Path),
    path: &str,
    start: u64,
) -> Result<u64, CliError> {
    let len = file.metadata().await.map_err(|e| local(from, &e))?.len();
    if len == 0 {
        let w =
            pb::FileWrite { path: path.into(), create: true, truncate: true, ..Default::default() };
        written(r.ask(Request::Write(Box::new(w))).await?)?;
        return Ok(0);
    }
    file.seek(std::io::SeekFrom::Start(start)).await.map_err(|e| local(from, &e))?;
    let (mut at, mut done) = (start, 0);
    while at < len || r.ahead() > 0 {
        while at < len && r.ahead() < AHEAD {
            let want = usize::try_from((len - at).min(CHUNK_U64)).unwrap_or(CHUNK);
            let mut data = vec![0u8; want];
            file.read_exact(&mut data).await.map_err(|e| local(from, &e))?;
            let w = pb::FileWrite {
                path: path.into(),
                offset: at,
                data,
                create: true,
                truncate: at == 0,
                ..Default::default()
            };
            r.send(Request::Write(Box::new(w))).await?;
            at += wide(want);
        }
        done += written(r.answer(path).await?)?;
    }
    Ok(done)
}

/// Reads `path`, of `size` bytes, into the local file `to`; continued from the local file's size
/// when `resume`, otherwise replacing it. How many bytes were read.
///
/// # Errors
/// The local file could not be written, the node refused a read, or the session ended.
pub async fn download(
    r: &mut Remote,
    path: &str,
    size: u64,
    (to, resume): (&Path, bool),
) -> Result<u64, CliError> {
    let mut options = tokio::fs::OpenOptions::new();
    options.create(true).write(true);
    if resume {
        options.append(true);
    } else {
        options.truncate(true);
    }
    let mut file = options.open(to).await.map_err(|e| local(to, &e))?;
    let start = if resume { file.metadata().await.map_err(|e| local(to, &e))?.len() } else { 0 };
    let (mut asked, mut done) = (start, 0u64);
    loop {
        while asked < size && r.ahead() < AHEAD {
            let read = pb::FileRead {
                path: path.into(),
                offset: asked,
                length: u32::try_from(CHUNK).unwrap_or(u32::MAX),
                ..Default::default()
            };
            r.send(Request::Read(Box::new(read))).await?;
            asked += CHUNK_U64;
        }
        if r.ahead() == 0 {
            break;
        }
        let data = match r.answer(path).await? {
            Response::Data(d) => d,
            other => return Err(CliError::Transport(format!("a read answered {other:?}"))),
        };
        // A file that shrank while it was read ends where it now ends; the reads still out are
        // answered empty and drained.
        file.write_all(&data).await.map_err(|e| local(to, &e))?;
        done += wide(data.len());
    }
    file.flush().await.map_err(|e| local(to, &e))?;
    Ok(done)
}
