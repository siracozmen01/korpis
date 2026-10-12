// SPDX-License-Identifier: AGPL-3.0-or-later

//! The files an artifact carries, checked against the document that names them.

use crate::support::PAPER;
use korpis_recipe::artifact::check_files;

fn titles(t: &[&str]) -> Vec<String> {
    t.iter().map(|&s| s.to_owned()).collect()
}

const FILES: [&str; 2] = ["layers/rootfs.tar.zst", "templates/server.properties"];

#[test]
fn every_named_file_present_is_enough() {
    let recipe = korpis_recipe::parse(PAPER.as_bytes()).unwrap();
    check_files(&recipe, &titles(&FILES)).unwrap();
    let mut extra = titles(&FILES);
    extra.push("README.md".to_owned());
    check_files(&recipe, &extra).unwrap();
}

#[test]
fn a_missing_file_is_named() {
    let recipe = korpis_recipe::parse(PAPER.as_bytes()).unwrap();
    let e = check_files(&recipe, &titles(&FILES[..1])).unwrap_err();
    assert!(e.0.contains("templates/server.properties"), "{e}");
}

#[test]
fn titles_are_clean_relative_paths_and_unique() {
    let recipe = korpis_recipe::parse(PAPER.as_bytes()).unwrap();
    for bad in ["../etc/passwd", "/etc/passwd", "./layers/rootfs.tar.zst", "a//b", "a/./b", ""] {
        let mut t = titles(&FILES);
        t.push(bad.to_owned());
        assert!(check_files(&recipe, &t).is_err(), "{bad:?} is not a file title");
    }
    let mut twice = titles(&FILES);
    twice.push(FILES[0].to_owned());
    assert!(check_files(&recipe, &twice).unwrap_err().0.contains("two files"));
}
