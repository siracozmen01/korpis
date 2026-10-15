// SPDX-License-Identifier: Apache-2.0

use super::*;
use rusty_s3::actions::S3Action as _;
use rusty_s3::{Bucket, Credentials, UrlStyle};

const KEY: &str = "KFTEST";
const SECRET: &str = "a secret of the target's own";

fn bucket() -> Bucket {
    let endpoint = "http://10.47.0.2:9000".parse().unwrap();
    Bucket::new(endpoint, UrlStyle::Path, "korpis", "local").unwrap()
}

fn credentials() -> Credentials {
    Credentials::new_with_token(KEY, SECRET, "the session")
}

/// A request for `url` as it arrives.
fn arrived(url: &url::Url) -> (String, String, Vec<(String, String)>) {
    let host = format!("{}:{}", url.host_str().unwrap(), url.port().unwrap());
    (url.path().to_owned(), url.query().unwrap().to_owned(), vec![("host".to_owned(), host)])
}

fn check(method: &str, url: &url::Url, now: SystemTime) -> Result<Verified, Refusal> {
    let (path, query, headers) = arrived(url);
    let r = Presigned { method, path: &path, query: &query, headers: &headers };
    verify_presigned(&r, now, |k| (k == KEY).then(|| SECRET.to_owned()))
}

#[test]
fn what_the_s3_client_presigns_verifies_and_nothing_else_does() {
    let (b, c) = (bucket(), credentials());
    let key = "workloads/wl_1/chunks/a b+c";
    let get = b.get_object(Some(&c), key).sign(Duration::from_secs(300));
    let now = SystemTime::now();
    let v = check("GET", &get, now).unwrap();
    assert_eq!((v.access_key_id.as_str(), v.session_token.as_str()), (KEY, "the session"));
    assert_eq!(check("PUT", &get, now), Err(Refusal::Signature), "another method");

    let mut list = b.list_objects_v2(Some(&c));
    list.with_prefix("workloads/wl_1/");
    list.with_continuation_token("workloads/wl_1/chunks/x");
    let listed = check("GET", &list.sign(Duration::from_secs(300)), now).unwrap();
    let prefix = listed.parameters.iter().find(|(k, _)| k == "prefix");
    assert_eq!(prefix.map(|(_, v)| v.as_str()), Some("workloads/wl_1/"));

    let put = b.put_object(Some(&c), key).sign(Duration::from_secs(300));
    let mut moved = put.clone();
    moved.set_path("/korpis/workloads/wl_2/chunks/x");
    assert_eq!(check("PUT", &moved, now), Err(Refusal::Signature), "another object");
    assert!(check("PUT", &put, now).is_ok());
    let later = now + Duration::from_secs(301 + 300);
    assert_eq!(check("PUT", &put, later), Err(Refusal::Expired));

    let (path, query, headers) = arrived(&put);
    let r = Presigned { method: "PUT", path: &path, query: &query, headers: &headers };
    assert_eq!(verify_presigned(&r, now, |_| None), Err(Refusal::UnknownKey));
    let wrong = verify_presigned(&r, now, |_| Some("another secret".to_owned()));
    assert_eq!(wrong, Err(Refusal::Signature));
    let no_host = Presigned { headers: &[], ..r };
    assert!(matches!(verify_presigned(&no_host, now, |_| None), Err(Refusal::Malformed(_))));
}

#[test]
fn a_date_is_read_as_the_instant_it_names() {
    let at = instant("20150830T123600Z").unwrap();
    assert_eq!(at.duration_since(UNIX_EPOCH).unwrap().as_secs(), 1_440_938_160);
    assert!(instant("20151330T123600Z").is_none() && instant("2015-08-30").is_none());
}
