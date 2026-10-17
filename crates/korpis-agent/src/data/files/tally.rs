// SPDX-License-Identifier: AGPL-3.0-or-later

//! What a file session changed, counted from each request and the answer to it, for the effect
//! the session's end records (§3.2 of docs/06-storage.md): how many files and bytes were written,
//! how many directories made, and every path removed or renamed, up to [`LISTED`], then only
//! their count. Reads change nothing and are not counted.

use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::file_request::Request;
use korpis_proto::korpis::v1::__buffa::oneof::file_response::Response;
use std::collections::{BTreeMap, BTreeSet};

/// The most paths an effect lists of each kind.
pub const LISTED: usize = 1000;
/// The most distinct paths written that are told apart; past it, each write to a path not among
/// them counts as another file, so a session of millions of files costs a bounded memory and
/// over-counts rather than under-counts.
const DISTINCT: usize = 10_000;

/// A request that may change something, as the answer to it is awaited.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pending {
    Write(String),
    Mkdir,
    Rename(String, String),
    Remove(String),
    Other,
}

impl Pending {
    /// What `request` would change.
    #[must_use]
    pub fn of(request: &pb::FileRequest) -> Self {
        match &request.request {
            Some(Request::Write(w)) => Self::Write(w.path.clone()),
            Some(Request::Mkdir(_)) => Self::Mkdir,
            Some(Request::Rename(r)) => Self::Rename(r.from.clone(), r.to.clone()),
            Some(Request::Remove(r)) => Self::Remove(r.path.clone()),
            _ => Self::Other,
        }
    }
}

/// What a session changed so far.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Tally {
    written: BTreeSet<String>,
    files: u64,
    bytes: u64,
    directories: u64,
    removed: Vec<String>,
    removed_count: u64,
    renamed: Vec<(String, String)>,
    renamed_count: u64,
}

fn text(s: &str) -> pb::Value {
    pb::Value { kind: Some(pb::value::Kind::StringValue(s.to_owned())), ..Default::default() }
}

fn number(n: u64) -> pb::Value {
    let n = i64::try_from(n).unwrap_or(i64::MAX);
    pb::Value { kind: Some(pb::value::Kind::IntValue(n)), ..Default::default() }
}

fn list(values: Vec<pb::Value>) -> pb::Value {
    let list = pb::ValueList { values, ..Default::default() };
    pb::Value { kind: Some(pb::value::Kind::ListValue(Box::new(list))), ..Default::default() }
}

impl Tally {
    /// Counts what `pending` changed, as `response` answered it.
    pub fn record(&mut self, pending: Pending, response: &pb::FileResponse) {
        match (pending, &response.response) {
            (Pending::Write(path), Some(Response::Written(n))) => {
                self.bytes = self.bytes.saturating_add(*n);
                if self.written.contains(&path) {
                    return;
                }
                self.files += 1;
                if self.written.len() < DISTINCT {
                    self.written.insert(path);
                }
            }
            (Pending::Mkdir, Some(Response::Done(true))) => self.directories += 1,
            (Pending::Remove(path), Some(Response::Done(true))) => {
                self.removed_count += 1;
                if self.removed.len() < LISTED {
                    self.removed.push(path);
                }
            }
            (Pending::Rename(from, to), Some(Response::Done(true))) => {
                self.renamed_count += 1;
                if self.renamed.len() < LISTED {
                    self.renamed.push((from, to));
                }
            }
            _ => {}
        }
    }

    /// The tally as an effect's fields.
    #[must_use]
    pub fn fields(&self) -> BTreeMap<String, pb::Value> {
        let removed = self.removed.iter().map(|p| text(p)).collect();
        let renamed =
            self.renamed.iter().map(|(from, to)| list(vec![text(from), text(to)])).collect();
        [
            ("files", number(self.files)),
            ("bytes", number(self.bytes)),
            ("directories", number(self.directories)),
            ("removed", list(removed)),
            ("removed_count", number(self.removed_count)),
            ("renamed", list(renamed)),
            ("renamed_count", number(self.renamed_count)),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v))
        .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::{LISTED, Pending, Tally, number};
    use korpis_proto::korpis::v1 as pb;
    use korpis_proto::korpis::v1::__buffa::oneof::file_response::Response;

    fn answer(r: Response) -> pb::FileResponse {
        pb::FileResponse { response: Some(r), ..Default::default() }
    }

    #[test]
    fn a_session_s_changes_are_counted_and_only_what_was_done() {
        let mut t = Tally::default();
        t.record(Pending::Write("/data/a".into()), &answer(Response::Written(5)));
        t.record(Pending::Write("/data/a".into()), &answer(Response::Written(3)));
        t.record(Pending::Write("/data/b".into()), &answer(Response::Written(1)));
        let refused = answer(Response::Error(Box::default()));
        t.record(Pending::Write("/data/c".into()), &refused);
        t.record(Pending::Remove("/data/old".into()), &refused);
        t.record(Pending::Mkdir, &answer(Response::Done(true)));
        t.record(
            Pending::Rename("/data/b".into(), "/data/d".into()),
            &answer(Response::Done(true)),
        );
        t.record(Pending::Other, &answer(Response::Data(vec![1])));
        let f = t.fields();
        assert_eq!(f["files"], number(2));
        assert_eq!(f["bytes"], number(9));
        assert_eq!(f["directories"], number(1));
        assert_eq!(f["removed_count"], number(0));
        assert_eq!(f["renamed_count"], number(1));
    }

    #[test]
    fn paths_past_the_listed_bound_are_counted_not_kept() {
        let mut t = Tally::default();
        for i in 0..LISTED + 5 {
            t.record(Pending::Remove(format!("/data/{i}")), &answer(Response::Done(true)));
        }
        assert_eq!(t.removed.len(), LISTED);
        assert_eq!(t.fields()["removed_count"], number(u64::try_from(LISTED + 5).unwrap_or(0)));
    }
}
