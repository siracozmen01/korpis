// SPDX-License-Identifier: AGPL-3.0-or-later

use super::*;

fn keys(p: &Page) -> Vec<&str> {
    p.objects.iter().map(|o| o.key.as_str()).collect()
}

#[test]
fn objects_round_trip_and_list_in_key_order_a_page_at_a_time() {
    let dir = tempfile::tempdir().unwrap();
    let s = Store::open(dir.path()).unwrap();
    // `-` sorts before `/`, so `a-b` comes before everything beneath `a/`.
    for k in ["w/a/2", "w/a-b", "w/a/1", "w/b", "x/other", "w/a/1x"] {
        s.put(k, k.as_bytes()).unwrap();
    }
    assert_eq!(s.get("w/a/1").unwrap().as_deref(), Some(&b"w/a/1"[..]));
    assert_eq!(s.get("w/a/9").unwrap(), None);
    assert_eq!(s.get("w/a").unwrap(), None, "a directory is no object");

    let all = s.list("w/", "", 10).unwrap();
    assert_eq!(keys(&all), ["w/a-b", "w/a/1", "w/a/1x", "w/a/2", "w/b"]);
    assert!(!all.more);
    let first = s.list("w/", "", 2).unwrap();
    assert_eq!((keys(&first), first.more), (vec!["w/a-b", "w/a/1"], true));
    let next = s.list("w/", "w/a/1", 2).unwrap();
    assert_eq!((keys(&next), next.more), (vec!["w/a/1x", "w/a/2"], true));
    assert_eq!(keys(&s.list("w/a/1", "", 10).unwrap()), ["w/a/1", "w/a/1x"], "a partial name");
    assert!(s.list("nothing/", "", 10).unwrap().objects.is_empty());

    assert_eq!(s.delete(&["w/a/1".into(), "w/a/1x".into(), "w/none".into()]).unwrap(), 2);
    assert_eq!(s.delete(&["w/a/2".into()]).unwrap(), 1);
    assert!(!dir.path().join("objects/w/a").exists(), "an emptied directory goes");
    assert!(dir.path().join("objects/w").exists(), "one still holding objects stays");
}

#[test]
fn a_name_that_could_leave_the_store_is_none() {
    let dir = tempfile::tempdir().unwrap();
    let s = Store::open(dir.path()).unwrap();
    for bad in ["", "/abs", "a//b", "a/../../etc", "./a", "a\0b"] {
        assert!(s.put(bad, b"x").is_err(), "{bad:?}");
        assert!(!is_key(bad));
    }
    assert!(s.list("../", "", 1).is_err());
}
