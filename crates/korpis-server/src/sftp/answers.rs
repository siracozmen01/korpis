// SPDX-License-Identifier: AGPL-3.0-or-later

//! What SFTP says for what the session says (§3.2 of docs/06-storage.md): paths beneath `/`, the
//! root the session sees, an entry's attributes and the line `ls -l` shows for it, and a refusal's
//! status.

use korpis_proto::korpis::v1 as pb;
use russh_sftp::protocol::{File, FileAttributes, Status, StatusCode};

/// `path` as the session names it: beneath `/`, a relative path taken from there, `.` and `..`
/// resolved by name, as `REALPATH` answers it.
pub(super) fn absolute(path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            name => parts.push(name),
        }
    }
    format!("/{}", parts.join("/"))
}

/// The mode an entry is shown with: its kind's type bits, and permissions that say what a file
/// session lets anyone do with it, since it sets none of its own.
fn mode(kind: pb::FileKind) -> u32 {
    match kind {
        pb::FileKind::FILE_KIND_DIRECTORY => 0o040_755,
        pb::FileKind::FILE_KIND_SYMLINK => 0o120_777,
        pb::FileKind::FILE_KIND_FILE => 0o100_644,
        _ => 0,
    }
}

/// An entry's attributes.
pub(super) fn attributes(e: &pb::FileEntry) -> FileAttributes {
    let kind = e.kind.as_known().unwrap_or(pb::FileKind::FILE_KIND_OTHER);
    let modified = e.modified.as_option().map(|t| u32::try_from(t.seconds).unwrap_or(0));
    FileAttributes {
        size: Some(e.size),
        permissions: Some(mode(kind)),
        atime: modified,
        mtime: modified,
        ..FileAttributes::default()
    }
}

/// The `ls -l` line for an entry, which a version 3 client prints as the server writes it.
fn long_name(e: &pb::FileEntry) -> String {
    let kind = e.kind.as_known().unwrap_or(pb::FileKind::FILE_KIND_OTHER);
    let letters = match kind {
        pb::FileKind::FILE_KIND_DIRECTORY => "drwxr-xr-x",
        pb::FileKind::FILE_KIND_SYMLINK => "lrwxrwxrwx",
        pb::FileKind::FILE_KIND_FILE => "-rw-r--r--",
        _ => "?---------",
    };
    let seconds = e.modified.as_option().map_or(0, |t| t.seconds);
    let when = jiff::Timestamp::from_second(seconds).unwrap_or(jiff::Timestamp::UNIX_EPOCH);
    let when = when.strftime("%b %e %H:%M");
    format!("{letters} 1 korpis korpis {:>12} {when} {}", e.size, e.name)
}

/// An entry as a directory listing names it.
pub(super) fn listed(e: &pb::FileEntry) -> File {
    File { filename: e.name.clone(), longname: long_name(e), attrs: attributes(e) }
}

/// A status answering request `id`.
pub(super) fn status(id: u32, code: StatusCode, message: &str) -> Status {
    let message = if message.is_empty() { code.to_string() } else { message.to_owned() };
    Status { id, status_code: code, error_message: message, language_tag: "en".to_owned() }
}

/// The status a refusal of the node's is.
pub(super) fn refused(id: u32, e: &pb::FileError) -> Status {
    use pb::FileErrorCode as F;
    let known = e.code.as_known().unwrap_or(F::FILE_ERROR_CODE_UNSPECIFIED);
    let code = match known {
        F::FILE_ERROR_CODE_NOT_FOUND => StatusCode::NoSuchFile,
        F::FILE_ERROR_CODE_REFUSED | F::FILE_ERROR_CODE_OUTSIDE => StatusCode::PermissionDenied,
        _ => StatusCode::Failure,
    };
    let message = match known {
        F::FILE_ERROR_CODE_FULL => "the volume is full",
        F::FILE_ERROR_CODE_EXISTS => "it exists already",
        F::FILE_ERROR_CODE_NOT_EMPTY => "the directory is not empty",
        F::FILE_ERROR_CODE_NOT_A_DIRECTORY => "not a directory",
        F::FILE_ERROR_CODE_IS_A_DIRECTORY => "a directory",
        _ => e.message.as_str(),
    };
    status(id, code, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_is_beneath_the_root_whatever_it_climbs() {
        assert_eq!(absolute("."), "/");
        assert_eq!(absolute(""), "/");
        assert_eq!(absolute("data/./world"), "/data/world");
        assert_eq!(absolute("/data//world/"), "/data/world");
        assert_eq!(absolute("../../etc/passwd"), "/etc/passwd");
        assert_eq!(absolute("/data/a/../b"), "/data/b");
    }

    #[test]
    fn an_entry_is_shown_with_its_kind_and_a_full_volume_says_so() {
        let e = pb::FileEntry {
            name: "world".into(),
            kind: pb::FileKind::FILE_KIND_DIRECTORY.into(),
            size: 4096,
            ..Default::default()
        };
        assert_eq!(attributes(&e).permissions, Some(0o040_755));
        assert!(listed(&e).longname.starts_with("drwxr-xr-x 1 korpis korpis         4096 Jan  1"));
        let full = pb::FileError {
            code: pb::FileErrorCode::FILE_ERROR_CODE_FULL.into(),
            ..Default::default()
        };
        let s = refused(7, &full);
        assert_eq!(
            (s.id, s.status_code, s.error_message.as_str()),
            (7, StatusCode::Failure, "the volume is full")
        );
        let ro = pb::FileError {
            code: pb::FileErrorCode::FILE_ERROR_CODE_REFUSED.into(),
            ..Default::default()
        };
        assert_eq!(refused(8, &ro).status_code, StatusCode::PermissionDenied);
    }
}
