// SPDX-License-Identifier: AGPL-3.0-or-later

use super::fs::{Fault, Kind, Root};
use rustix::fs::{Mode, OFlags};

fn root(dir: &std::path::Path, write: bool) -> Root {
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC;
    Root::new(rustix::fs::open(dir, flags, Mode::empty()).unwrap(), write)
}

#[test]
fn a_file_is_written_in_chunks_read_back_and_resumed_from_its_size() {
    let dir = tempfile::tempdir().unwrap();
    let r = root(dir.path(), true);
    r.mkdir("/world").unwrap();
    assert_eq!(r.write("/world/level.dat", 0, b"hello ", true, true).unwrap(), 6);
    let resumed = r.stat("/world/level.dat").unwrap().size;
    assert_eq!(r.write("/world/level.dat", resumed, b"world", false, false).unwrap(), 5);
    assert_eq!(r.read("/world/level.dat", 0, 64).unwrap(), b"hello world");
    assert_eq!(r.read("/world/level.dat", 6, 3).unwrap(), b"wor");
    let (entries, next) = r.list("/", "", 10).unwrap();
    assert_eq!(
        (entries[0].name.as_str(), entries[0].kind, next.as_str()),
        ("world", Kind::Directory, "")
    );
}

#[test]
fn a_listing_comes_a_page_at_a_time_in_name_order() {
    let dir = tempfile::tempdir().unwrap();
    let r = root(dir.path(), true);
    for n in ["c", "a", "b"] {
        r.write(&format!("/{n}"), 0, b"x", true, false).unwrap();
    }
    let (page, next) = r.list("/", "", 2).unwrap();
    assert_eq!(
        (page.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(), next.as_str()),
        (vec!["a", "b"], "b")
    );
    let (rest, end) = r.list("/", &next, 2).unwrap();
    assert_eq!((rest.len(), rest[0].name.as_str(), end.as_str()), (1, "c", ""));
}

#[test]
fn nothing_outside_the_root_is_reached_by_a_link_or_by_climbing() {
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("secret"), b"host").unwrap();
    let dir = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(outside.path().join("secret"), dir.path().join("planted")).unwrap();
    std::os::unix::fs::symlink(outside.path(), dir.path().join("door")).unwrap();
    let r = root(dir.path(), true);
    assert_eq!(r.read("/planted", 0, 10).unwrap_err(), Fault::Outside);
    assert_eq!(r.read("/door/secret", 0, 10).unwrap_err(), Fault::Outside);
    assert_eq!(r.read("/../secret", 0, 10).unwrap_err(), Fault::Outside);
    assert_eq!(r.write("/planted", 0, b"x", false, false).unwrap_err(), Fault::Outside);
    assert_eq!(r.stat("/planted").unwrap().kind, Kind::Symlink, "shown, never followed");
    assert_eq!(std::fs::read(outside.path().join("secret")).unwrap(), b"host");
}

#[test]
fn a_session_to_read_changes_nothing_and_a_rename_never_replaces() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a"), b"1").unwrap();
    std::fs::write(dir.path().join("b"), b"2").unwrap();
    let reading = root(dir.path(), false);
    assert_eq!(reading.write("/a", 0, b"x", false, false).unwrap_err(), Fault::Refused);
    assert_eq!(reading.remove("/a", false).unwrap_err(), Fault::Refused);
    let r = root(dir.path(), true);
    assert_eq!(r.rename("/a", "/b").unwrap_err(), Fault::Exists);
    r.rename("/a", "/c").unwrap();
    r.mkdir("/d").unwrap();
    r.write("/d/e", 0, b"x", true, false).unwrap();
    assert_eq!(r.remove("/d", false).unwrap_err(), Fault::NotEmpty);
    assert_eq!(r.remove("/d", true).unwrap(), 2);
    assert_eq!(r.stat("/d").unwrap_err(), Fault::NotFound);
}

#[test]
fn a_fifo_is_refused_not_waited_on_and_a_tree_too_deep_is_refused_not_walked() {
    let dir = tempfile::tempdir().unwrap();
    rustix::fs::mknodat(
        rustix::fs::CWD,
        dir.path().join("pipe"),
        rustix::fs::FileType::Fifo,
        Mode::from_raw_mode(0o644),
        0,
    )
    .unwrap();
    let r = root(dir.path(), true);
    assert_eq!(r.read("/pipe", 0, 10).unwrap_err(), Fault::Refused, "and it returned at once");
    assert_eq!(r.write("/pipe", 0, b"x", false, false).unwrap_err(), Fault::Refused);
    let mut deep = String::new();
    for _ in 0..=super::fs::DEPTH {
        deep.push_str("/d");
        r.mkdir(&deep).unwrap();
    }
    assert_eq!(r.remove("/d", true).unwrap_err(), Fault::Refused);
    assert_eq!(r.stat("/..").unwrap_err(), Fault::Refused, "never the root's metadata");
    assert_eq!(r.stat("/").unwrap().kind, Kind::Directory);
}
