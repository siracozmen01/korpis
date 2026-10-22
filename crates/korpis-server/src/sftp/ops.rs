// SPDX-License-Identifier: AGPL-3.0-or-later

//! The requests answered one at a time, each by the verbs §3.2 of docs/06-storage.md maps it to:
//! a stat by `stat`, a directory read by pages of `list`, an open by a `stat` or by the empty
//! write that creates or truncates, a remove by `remove` once a `stat` has said the entry is of
//! the kind the request removes. What the session cannot do is refused as unsupported.

use super::Authorizer;
use super::answers::{absolute, attributes, listed, refused, status};
use super::files::{Files, Open};
use super::node::Lost;
use crate::relay::Verifier;
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::file_request::Request;
use korpis_proto::korpis::v1::__buffa::oneof::file_response::Response;
use russh_sftp::protocol::{
    Attrs, File, Handle, Name, OpenFlags, Packet, Status, StatusCode, Version, Write,
};

/// The most handles a connection holds open.
const HANDLES: usize = 64;
/// How many entries a page of a directory asks for.
const PAGE: u32 = 128;

/// A reply, or the status refusing the request.
type Reply = Result<Packet, Status>;

fn wrong() -> Lost {
    "an answer of another kind than its request".to_owned()
}

fn is_dir(e: &pb::FileEntry) -> bool {
    e.kind.as_known() == Some(pb::FileKind::FILE_KIND_DIRECTORY)
}

fn ok(id: u32) -> Packet {
    Packet::Status(status(id, StatusCode::Ok, ""))
}

fn no_handle(id: u32) -> Status {
    status(id, StatusCode::Failure, "no such handle")
}

impl<V: Verifier> Files<V> {
    /// The reply to `packet`, with nothing ahead of it.
    pub(super) async fn serially<A: Authorizer>(
        &mut self,
        packet: Packet,
        a: &A,
    ) -> Result<Packet, Lost> {
        let id = packet.get_request_id();
        let reply = match packet {
            Packet::Init(_) => Ok(Packet::Version(Version::new())),
            Packet::RealPath(p) => {
                Ok(Packet::Name(Name { id, files: vec![File::dummy(absolute(&p.path))] }))
            }
            Packet::Stat(s) => self.stat(id, &s.path, a).await?,
            Packet::Lstat(s) => self.stat(id, &s.path, a).await?,
            Packet::Fstat(f) => match self.path_of(&f.handle) {
                Some(path) => self.stat(id, &path, a).await?,
                None => Err(no_handle(id)),
            },
            Packet::Open(o) => self.open(id, &o.filename, o.pflags, a).await?,
            Packet::Close(c) => self.handles.remove(&c.handle).map(|_| ok(id)).ok_or(no_handle(id)),
            Packet::OpenDir(d) => self.open_dir(id, &d.path, a).await?,
            Packet::ReadDir(d) => self.read_dir(id, &d.handle, a).await?,
            Packet::Remove(r) => self.remove(id, &r.filename, false, a).await?,
            Packet::RmDir(r) => self.remove(id, &r.path, true, a).await?,
            Packet::MkDir(m) => {
                let mkdir = pb::FileMkdir { path: absolute(&m.path), ..Default::default() };
                self.done(id, Request::Mkdir(Box::new(mkdir)), a).await?
            }
            Packet::Rename(r) => {
                let (from, to) = (absolute(&r.oldpath), absolute(&r.newpath));
                let rename = pb::FileRename { from, to, ..Default::default() };
                self.done(id, Request::Rename(Box::new(rename)), a).await?
            }
            Packet::Write(w) => self.append(w, a).await?,
            Packet::Read(_) => Err(no_handle(id)),
            Packet::SetStat(_) | Packet::FSetStat(_) => Err(status(
                id,
                StatusCode::OpUnsupported,
                "a file session sets no permission, owner, or time",
            )),
            Packet::Symlink(_) | Packet::ReadLink(_) => Err(status(
                id,
                StatusCode::OpUnsupported,
                "a link is listed, never followed or made",
            )),
            Packet::Extended(_) => Err(status(id, StatusCode::OpUnsupported, "")),
            _ => Err(status(id, StatusCode::BadMessage, "")),
        };
        Ok(reply.unwrap_or_else(Packet::Status))
    }

    fn path_of(&self, handle: &str) -> Option<String> {
        match self.handles.get(handle)? {
            Open::File { path, .. } | Open::Dir { path, .. } => Some(path.clone()),
        }
    }

    fn hold(&mut self, open: Open) -> String {
        self.next_handle += 1;
        let handle = self.next_handle.to_string();
        self.handles.insert(handle.clone(), open);
        handle
    }

    /// The entry at `path`, or the node's refusal.
    async fn entry<A: Authorizer>(
        &mut self,
        path: &str,
        a: &A,
    ) -> Result<Result<pb::FileEntry, pb::FileError>, Lost> {
        let stat = pb::FileStat { path: path.to_owned(), ..Default::default() };
        match self.session.ask(Request::Stat(Box::new(stat)), a).await? {
            Ok(Response::Entry(e)) => Ok(Ok(*e)),
            Ok(_) => Err(wrong()),
            Err(e) => Ok(Err(e)),
        }
    }

    async fn stat<A: Authorizer>(&mut self, id: u32, path: &str, a: &A) -> Result<Reply, Lost> {
        Ok(match self.entry(&absolute(path), a).await? {
            Ok(e) => Ok(Packet::Attrs(Attrs { id, attrs: attributes(&e) })),
            Err(e) => Err(refused(id, &e)),
        })
    }

    /// A request the node answers with `done`.
    async fn done<A: Authorizer>(&mut self, id: u32, r: Request, a: &A) -> Result<Reply, Lost> {
        match self.session.ask(r, a).await? {
            Ok(Response::Done(_)) => Ok(Ok(ok(id))),
            Ok(_) => Err(wrong()),
            Err(e) => Ok(Err(refused(id, &e))),
        }
    }

    /// Opens `name` as `flags` ask: created, or emptied, by an empty write, which is refused in
    /// a session that only reads; otherwise looked at, so a handle names a file that is there.
    /// `EXCLUDE` is a look first, which another writer can race; the session has no verb that
    /// creates only what is absent.
    async fn open<A: Authorizer>(
        &mut self,
        id: u32,
        name: &str,
        flags: OpenFlags,
        a: &A,
    ) -> Result<Reply, Lost> {
        let path = absolute(name);
        let changes =
            OpenFlags::WRITE | OpenFlags::APPEND | OpenFlags::CREATE | OpenFlags::TRUNCATE;
        let write = flags.intersects(changes);
        if write && !self.write {
            let why = "this key may read the workload's files, not change them";
            return Ok(Err(status(id, StatusCode::PermissionDenied, why)));
        }
        if self.handles.len() >= HANDLES {
            return Ok(Err(status(id, StatusCode::Failure, "too many open handles")));
        }
        if flags.contains(OpenFlags::CREATE | OpenFlags::EXCLUDE) {
            match self.entry(&path, a).await? {
                Ok(_) => return Ok(Err(status(id, StatusCode::Failure, "it exists already"))),
                Err(e)
                    if e.code.as_known() == Some(pb::FileErrorCode::FILE_ERROR_CODE_NOT_FOUND) => {}
                Err(e) => return Ok(Err(refused(id, &e))),
            }
        }
        if flags.intersects(OpenFlags::CREATE | OpenFlags::TRUNCATE) {
            let empty = pb::FileWrite {
                path: path.clone(),
                create: flags.contains(OpenFlags::CREATE),
                truncate: flags.contains(OpenFlags::TRUNCATE),
                ..Default::default()
            };
            match self.session.ask(Request::Write(Box::new(empty)), a).await? {
                Ok(Response::Written(_)) => {}
                Ok(_) => return Err(wrong()),
                Err(e) => return Ok(Err(refused(id, &e))),
            }
        } else {
            match self.entry(&path, a).await? {
                Ok(e) if is_dir(&e) => {
                    return Ok(Err(status(id, StatusCode::Failure, "a directory")));
                }
                Ok(_) => {}
                Err(e) => return Ok(Err(refused(id, &e))),
            }
        }
        let append = flags.contains(OpenFlags::APPEND);
        let handle = self.hold(Open::File { path, write, append });
        Ok(Ok(Packet::Handle(Handle { id, handle })))
    }

    async fn open_dir<A: Authorizer>(&mut self, id: u32, name: &str, a: &A) -> Result<Reply, Lost> {
        let path = absolute(name);
        match self.entry(&path, a).await? {
            Ok(e) if !is_dir(&e) => Ok(Err(status(id, StatusCode::Failure, "not a directory"))),
            Ok(_) if self.handles.len() >= HANDLES => {
                Ok(Err(status(id, StatusCode::Failure, "too many open handles")))
            }
            Ok(_) => {
                let handle = self.hold(Open::Dir { path, after: String::new(), done: false });
                Ok(Ok(Packet::Handle(Handle { id, handle })))
            }
            Err(e) => Ok(Err(refused(id, &e))),
        }
    }

    /// The next page of the directory `handle` lists; the end of it once the last was read.
    async fn read_dir<A: Authorizer>(
        &mut self,
        id: u32,
        handle: &str,
        a: &A,
    ) -> Result<Reply, Lost> {
        let (path, after) = match self.handles.get(handle) {
            Some(Open::Dir { done: true, .. }) => return Ok(Err(status(id, StatusCode::Eof, ""))),
            Some(Open::Dir { path, after, .. }) => (path.clone(), after.clone()),
            _ => return Ok(Err(no_handle(id))),
        };
        let list = pb::FileList { path, after, limit: PAGE, ..Default::default() };
        let page = match self.session.ask(Request::List(Box::new(list)), a).await? {
            Ok(Response::Entries(page)) => page,
            Ok(_) => return Err(wrong()),
            Err(e) => return Ok(Err(refused(id, &e))),
        };
        if let Some(Open::Dir { after, done, .. }) = self.handles.get_mut(handle) {
            *done = page.next.is_empty();
            after.clone_from(&page.next);
        }
        if page.entries.is_empty() {
            return Ok(Err(status(id, StatusCode::Eof, "")));
        }
        Ok(Ok(Packet::Name(Name { id, files: page.entries.iter().map(listed).collect() })))
    }

    /// Removes `name`, a directory when `dir` and anything else otherwise, a directory only once
    /// it is empty, as version 3 has `REMOVE` and `RMDIR` do.
    async fn remove<A: Authorizer>(
        &mut self,
        id: u32,
        name: &str,
        dir: bool,
        a: &A,
    ) -> Result<Reply, Lost> {
        let path = absolute(name);
        match self.entry(&path, a).await? {
            Ok(e) if is_dir(&e) && !dir => {
                return Ok(Err(status(
                    id,
                    StatusCode::Failure,
                    "a directory, which RMDIR removes",
                )));
            }
            Ok(e) if !is_dir(&e) && dir => {
                return Ok(Err(status(id, StatusCode::Failure, "not a directory")));
            }
            Ok(_) => {}
            Err(e) => return Ok(Err(refused(id, &e))),
        }
        let remove = pb::FileRemove { path, recursive: false, ..Default::default() };
        self.done(id, Request::Remove(Box::new(remove)), a).await
    }

    /// A write to a file opened to append: at its end, wherever the client said.
    async fn append<A: Authorizer>(&mut self, w: Write, a: &A) -> Result<Reply, Lost> {
        let path = match self.handles.get(&w.handle) {
            Some(Open::File { path, write: true, .. }) => path.clone(),
            Some(Open::File { .. }) => {
                return Ok(Err(status(w.id, StatusCode::PermissionDenied, "opened to read")));
            }
            _ => return Ok(Err(no_handle(w.id))),
        };
        let offset = match self.entry(&path, a).await? {
            Ok(e) => e.size,
            Err(e) => return Ok(Err(refused(w.id, &e))),
        };
        let write = pb::FileWrite { path, offset, data: w.data, ..Default::default() };
        match self.session.ask(Request::Write(Box::new(write)), a).await? {
            Ok(Response::Written(_)) => Ok(Ok(ok(w.id))),
            Ok(_) => Err(wrong()),
            Err(e) => Ok(Err(refused(w.id, &e))),
        }
    }
}
