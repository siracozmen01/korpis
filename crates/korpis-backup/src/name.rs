// SPDX-License-Identifier: AGPL-3.0-or-later

//! What a tree may name. A restore creates every name a tree holds, so a tree is checked before
//! anything is created from it: a name that is empty, `.` or `..`, or holds `/` or NUL would
//! reach outside the directory being restored, and two entries of one name would make the second
//! land on what the first created.

use crate::error::{Error, Result};
use korpis_proto::korpis::backup::v1 as pb;

/// That `name` names one entry within its directory.
pub fn check_name(name: &[u8]) -> Result<()> {
    let ok = !name.is_empty()
        && name != b"."
        && name != b".."
        && !name.contains(&b'/')
        && !name.contains(&0);
    if ok {
        Ok(())
    } else {
        Err(Error::Corrupt(format!("an entry named {:?}", String::from_utf8_lossy(name))))
    }
}

/// That every entry of `tree` has a name of its own, in order.
pub fn check_tree(tree: &pb::Tree) -> Result<()> {
    let mut last: Option<&[u8]> = None;
    for e in &tree.entries {
        check_name(&e.name)?;
        if last.is_some_and(|l| l >= e.name.as_slice()) {
            return Err(Error::Corrupt("a tree's entries are not in order, or repeat".into()));
        }
        last = Some(&e.name);
    }
    Ok(())
}

/// A name for messages.
#[must_use]
pub fn display(name: &[u8]) -> String {
    String::from_utf8_lossy(name).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &[u8]) -> pb::Entry {
        pb::Entry { name: name.to_vec(), ..Default::default() }
    }

    #[test]
    fn a_tree_names_only_its_own_entries_once_each() {
        for bad in [&b""[..], b".", b"..", b"a/b", b"a\0"] {
            assert!(check_name(bad).is_err(), "{bad:?}");
        }
        assert!(check_name(b"..a").is_ok());
        let ok = pb::Tree { entries: vec![entry(b"a"), entry(b"b")], ..Default::default() };
        assert!(check_tree(&ok).is_ok());
        let twice = pb::Tree { entries: vec![entry(b"a"), entry(b"a")], ..Default::default() };
        assert!(check_tree(&twice).is_err());
        let back = pb::Tree { entries: vec![entry(b"b"), entry(b"a")], ..Default::default() };
        assert!(check_tree(&back).is_err());
    }
}
