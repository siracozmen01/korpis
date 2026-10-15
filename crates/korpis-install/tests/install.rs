// SPDX-License-Identifier: AGPL-3.0-or-later

//! The install actions on directories standing in for volumes: every verb, run twice to the same
//! result; links a tenant planted, refused rather than followed out; a resume at the action that
//! failed; and a fetch that reaches only what fetches downloaded.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "a failing test should fail loudly"
)]

mod bench;

use bench::{Bench, act, mode};
use korpis_core::recipe::install::{Hash, InstallStep};
use korpis_core::recipe::installing::InstallAction;
use korpis_core::recipe::template::{Context, TemplateFormat, Var};
use std::path::Path;

/// One of each verb but `extract` and `chown`, which have tests of their own.
fn every_verb(jar: &str) -> Vec<InstallAction> {
    let context: Context = [
        ("motd".to_owned(), Var::Text("Hi =x".into())),
        ("max".to_owned(), Var::Number("20".into())),
    ]
    .into();
    vec![
        act(InstallStep::Mkdir { path: "/data/plugins".into(), mode: 0o750 }),
        act(InstallStep::Fetch {
            url: "https://example.org/s.jar".into(),
            hash: Hash::parse(jar).unwrap(),
            to: "/data/bin/server.jar".into(),
        }),
        act(InstallStep::Copy { from: "motd.txt".into(), to: "/data/motd.txt".into() }),
        act(InstallStep::Write {
            path: "/data/eula.txt".into(),
            content: "eula=true\n".into(),
            mode: 0o640,
        }),
        InstallAction {
            step: InstallStep::Template {
                from: "templates/server.properties".into(),
                to: "/data/server.properties".into(),
                format: TemplateFormat::Properties,
                vars: vec!["motd".into(), "max".into()],
            },
            context: Some(context),
        },
        act(InstallStep::Chmod { path: "/data/bin/server.jar".into(), mode: 0o600 }),
        act(InstallStep::Symlink {
            target: "bin/server.jar".into(),
            link: "/data/server.jar".into(),
        }),
        act(InstallStep::Delete { path: "/data/old.log".into() }),
        act(InstallStep::Delete { path: "/data/never-there".into() }),
    ]
}

#[test]
fn every_verb_writes_beneath_its_volume_and_running_again_gives_the_same_result() {
    let mut b = Bench::new();
    let jar = b.keep(b"the server");
    b.file("templates/server.properties", b"motd={{motd}}\nmax={{max}}\n");
    b.file("motd.txt", b"welcome\n");
    std::fs::write(b.at("/data/old.log"), b"stale").unwrap();
    let actions = every_verb(&jar);
    for round in 0..2 {
        assert_eq!(
            b.run(&actions, 0).unwrap(),
            (0..actions.len()).collect::<Vec<_>>(),
            "round {round}"
        );
        assert_eq!(mode(&b.at("/data/plugins")), 0o750);
        assert_eq!(std::fs::read(b.at("/data/bin/server.jar")).unwrap(), b"the server");
        assert_eq!(mode(&b.at("/data/bin/server.jar")), 0o600);
        assert_eq!(std::fs::read(b.at("/data/motd.txt")).unwrap(), b"welcome\n");
        assert_eq!(mode(&b.at("/data/eula.txt")), 0o640);
        let props = std::fs::read_to_string(b.at("/data/server.properties")).unwrap();
        assert_eq!(props, "motd=Hi =x\nmax=20\n");
        assert_eq!(
            std::fs::read_link(b.at("/data/server.jar")).unwrap(),
            Path::new("bin/server.jar")
        );
        assert!(!b.at("/data/old.log").exists());
    }
    let left: Vec<_> = std::fs::read_dir(&b.volume)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().starts_with(".korpis-"))
        .collect();
    assert!(left.is_empty(), "nothing half-made is left: {left:?}");
}

#[test]
fn a_link_the_tenant_planted_never_carries_a_verb_out_of_the_volume() {
    let b = Bench::new();
    std::os::unix::fs::symlink(&b.outside, b.at("/data/escape")).unwrap();
    std::os::unix::fs::symlink("../outside", b.at("/data/up")).unwrap();
    std::fs::write(b.outside.join("secret"), b"host").unwrap();
    std::os::unix::fs::symlink(b.outside.join("secret"), b.at("/data/secret")).unwrap();
    for path in ["/data/escape/x", "/data/up/x"] {
        let w = act(InstallStep::Write { path: path.into(), content: "x".into(), mode: 0o644 });
        assert!(b.run(&[w], 0).is_err(), "{path}");
    }
    let chmod = act(InstallStep::Chmod { path: "/data/secret".into(), mode: 0o777 });
    assert!(b.run(&[chmod], 0).is_err(), "a mode is never set through a link");
    let extract = act(InstallStep::Extract {
        from: "/data/secret".into(),
        to: "/data/x".into(),
        strip: 0,
        include: vec![],
    });
    assert!(b.run(&[extract], 0).is_err(), "nor is anything read through one");
    let replace =
        act(InstallStep::Symlink { target: "/data/x".into(), link: "/data/plain".into() });
    std::fs::write(b.at("/data/plain"), b"a file").unwrap();
    assert!(b.run(&[replace], 0).is_err(), "a link replaces a link, nothing else");
    let out = act(InstallStep::Symlink { target: "../../etc".into(), link: "/data/l".into() });
    assert!(b.run(&[out], 0).is_err(), "a link verb points inside its volume");
    assert_eq!(std::fs::read_dir(&b.outside).unwrap().count(), 1, "nothing written outside");
    assert_eq!(mode(&b.outside.join("secret")) & 0o777, mode(&b.outside.join("secret")) & 0o777);
    let through = act(InstallStep::Delete { path: "/data/escape/secret".into() });
    assert!(b.run(&[through], 0).is_err(), "a link out is not an absence");
    assert!(b.outside.join("secret").exists());
    let root = act(InstallStep::Delete { path: "/data".into() });
    assert!(b.run(&[root], 0).is_err(), "a volume's root is not an install's to remove");
}

#[test]
fn an_install_resumes_at_the_action_that_failed() {
    let mut b = Bench::new();
    let actions = vec![
        act(InstallStep::Write { path: "/data/a".into(), content: "a".into(), mode: 0o644 }),
        act(InstallStep::Copy { from: "late.txt".into(), to: "/data/late".into() }),
        act(InstallStep::Write { path: "/data/c".into(), content: "c".into(), mode: 0o644 }),
    ];
    let e = b.run(&actions, 0).unwrap_err();
    assert_eq!(e.index, 1);
    b.file("late.txt", b"now here");
    assert_eq!(b.run(&actions, e.index).unwrap(), [1, 2], "from the one that failed");
    assert_eq!(std::fs::read(b.at("/data/late")).unwrap(), b"now here");
}

#[test]
fn an_owner_is_set_as_the_workload_sees_it_when_the_worker_may() {
    if !rustix::process::geteuid().is_root() {
        return;
    }
    let b = Bench::new();
    let actions = vec![
        act(InstallStep::Write { path: "/data/owned".into(), content: "x".into(), mode: 0o644 }),
        act(InstallStep::Chown { path: "/data/owned".into(), uid: 1000, gid: 1001 }),
    ];
    b.run(&actions, 0).unwrap();
    let st = std::fs::metadata(b.at("/data/owned")).unwrap();
    assert_eq!(
        (std::os::unix::fs::MetadataExt::uid(&st), std::os::unix::fs::MetadataExt::gid(&st)),
        (1000, 1001)
    );
}

#[test]
fn a_fetch_reaches_only_what_fetches_downloaded_never_a_recipe_s_file() {
    let mut b = Bench::new();
    // Another tenant's recipe file, from a private registry, on the same node.
    let private = b.file("secret.cfg", b"another tenant's private file");
    b.files.clear();
    let hash = Hash::parse(&private).unwrap();
    let url = "https://example.org/anything".to_owned();
    let fetch = act(InstallStep::Fetch { url, hash, to: "/data/leak".into() });
    let e = b.run(&[fetch], 0).unwrap_err();
    assert!(e.reason.contains("not in the content store"), "{e:?}");
    assert!(!b.at("/data/leak").exists());
}
