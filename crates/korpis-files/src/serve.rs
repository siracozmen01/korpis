// SPDX-License-Identifier: AGPL-3.0-or-later

//! A file session's requests answered (`files.proto`): each `FileRequest` is one verb on the
//! root, and its `FileResponse` carries what it returned or why it was not done. A refused
//! request ends nothing; the session goes on to the next.

use crate::fs::{CHUNK, Entry, Fault, Kind, Root};
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::file_request::Request;
use korpis_proto::korpis::v1::__buffa::oneof::file_response::Response;

fn code(f: Fault) -> pb::FileErrorCode {
    match f {
        Fault::NotFound => pb::FileErrorCode::FILE_ERROR_CODE_NOT_FOUND,
        Fault::Exists => pb::FileErrorCode::FILE_ERROR_CODE_EXISTS,
        Fault::Full => pb::FileErrorCode::FILE_ERROR_CODE_FULL,
        Fault::NotADirectory => pb::FileErrorCode::FILE_ERROR_CODE_NOT_A_DIRECTORY,
        Fault::IsADirectory => pb::FileErrorCode::FILE_ERROR_CODE_IS_A_DIRECTORY,
        Fault::NotEmpty => pb::FileErrorCode::FILE_ERROR_CODE_NOT_EMPTY,
        Fault::Outside => pb::FileErrorCode::FILE_ERROR_CODE_OUTSIDE,
        Fault::Refused => pb::FileErrorCode::FILE_ERROR_CODE_REFUSED,
        Fault::Io => pb::FileErrorCode::FILE_ERROR_CODE_IO,
    }
}

/// What a fault says to the person who asked.
fn says(f: Fault) -> &'static str {
    match f {
        Fault::NotFound => "no such file or directory",
        Fault::Exists => "it exists already",
        Fault::Full => "the volume is full",
        Fault::NotADirectory => "a part of the path is not a directory",
        Fault::IsADirectory => "it is a directory",
        Fault::NotEmpty => "the directory is not empty",
        Fault::Outside => "outside the workload's volumes",
        Fault::Refused => "not allowed in this session, or too large",
        Fault::Io => "the filesystem failed",
    }
}

fn entry(e: Entry) -> pb::FileEntry {
    let kind = match e.kind {
        Kind::File => pb::FileKind::FILE_KIND_FILE,
        Kind::Directory => pb::FileKind::FILE_KIND_DIRECTORY,
        Kind::Symlink => pb::FileKind::FILE_KIND_SYMLINK,
        Kind::Other => pb::FileKind::FILE_KIND_OTHER,
    };
    let modified =
        buffa_types::google::protobuf::Timestamp { seconds: e.modified, ..Default::default() };
    pb::FileEntry {
        name: e.name,
        kind: kind.into(),
        size: e.size,
        modified: Some(modified).into(),
        ..Default::default()
    }
}

/// The answer to `request` on `root`.
#[must_use]
pub fn answer(root: &Root, request: pb::FileRequest) -> pb::FileResponse {
    let response = match done(root, request.request) {
        Ok(r) => r,
        Err(f) => {
            let error = pb::FileError {
                code: code(f).into(),
                message: says(f).to_owned(),
                ..Default::default()
            };
            Response::Error(Box::new(error))
        }
    };
    pb::FileResponse { id: request.id, response: Some(response), ..Default::default() }
}

fn done(root: &Root, request: Option<Request>) -> Result<Response, Fault> {
    match request.ok_or(Fault::Refused)? {
        Request::List(l) => {
            let limit = usize::try_from(l.limit).unwrap_or(usize::MAX);
            let (entries, next) = root.list(&l.path, &l.after, limit)?;
            let entries = entries.into_iter().map(entry).collect();
            Ok(Response::Entries(Box::new(pb::FileEntries { entries, next, ..Default::default() })))
        }
        Request::Stat(s) => Ok(Response::Entry(Box::new(entry(root.stat(&s.path)?)))),
        Request::Read(r) => {
            let length = usize::try_from(r.length).unwrap_or(CHUNK);
            Ok(Response::Data(root.read(&r.path, r.offset, length)?))
        }
        Request::Write(w) => {
            Ok(Response::Written(root.write(&w.path, w.offset, &w.data, w.create, w.truncate)?))
        }
        Request::Mkdir(m) => root.mkdir(&m.path).map(|()| Response::Done(true)),
        Request::Rename(r) => root.rename(&r.from, &r.to).map(|()| Response::Done(true)),
        Request::Remove(r) => root.remove(&r.path, r.recursive).map(|_| Response::Done(true)),
    }
}
