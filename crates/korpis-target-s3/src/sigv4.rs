// SPDX-License-Identifier: AGPL-3.0-or-later

//! AWS Signature Version 4, for a request whose headers carry the signature: what a token
//! service takes, which the S3 client's presigned addresses do not cover. Only what that needs:
//! the canonical request over the method, the path, the query, the signed headers in order, and
//! the payload's hash; the string to sign over its scope; and the derived key.

use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use std::fmt::Write as _;

/// Who signs, where, for what.
#[derive(Debug, Clone, Copy)]
pub struct Signer<'a> {
    /// The access key.
    pub access_key_id: &'a str,
    /// Its secret.
    pub secret: &'a str,
    /// The region.
    pub region: &'a str,
    /// The service, `sts` or `s3`.
    pub service: &'a str,
}

/// A request to sign.
#[derive(Debug, Clone, Copy)]
pub struct Request<'a> {
    /// `GET` or `POST`.
    pub method: &'a str,
    /// The path, already encoded.
    pub path: &'a str,
    /// The query, already canonical: sorted, each name and value encoded.
    pub query: &'a str,
    /// The headers signed, lower-case names in order, with their values.
    pub headers: &'a [(&'a str, &'a str)],
    /// The payload.
    pub body: &'a [u8],
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

fn sha256(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

/// HMAC-SHA256 of `data` under `key`. HMAC takes a key of any length, so the empty answer for a
/// refused key never happens; were it to, the signature would be refused by the service.
fn hmac(key: &[u8], data: &str) -> Vec<u8> {
    Hmac::<Sha256>::new_from_slice(key).map_or_else(
        |_| Vec::new(),
        |mut mac| {
            mac.update(data.as_bytes());
            mac.finalize().into_bytes().to_vec()
        },
    )
}

impl Signer<'_> {
    /// The canonical request of `r`.
    #[must_use]
    pub fn canonical(r: &Request<'_>) -> String {
        let headers = r.headers.iter().fold(String::new(), |mut s, (n, v)| {
            let _ = writeln!(s, "{n}:{}", v.trim());
            s
        });
        let names: Vec<&str> = r.headers.iter().map(|(n, _)| *n).collect();
        format!(
            "{}\n{}\n{}\n{headers}\n{}\n{}",
            r.method,
            r.path,
            r.query,
            names.join(";"),
            sha256(r.body)
        )
    }

    /// The `Authorization` header for `r` at `amz_date` (`YYYYMMDDTHHMMSSZ`), which `r` signs as
    /// its `x-amz-date` header.
    #[must_use]
    pub fn authorization(&self, r: &Request<'_>, amz_date: &str) -> String {
        let day = amz_date.get(..8).unwrap_or_default();
        let scope = format!("{day}/{}/{}/aws4_request", self.region, self.service);
        let to_sign = format!(
            "AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{}",
            sha256(Self::canonical(r).as_bytes())
        );
        let key = [day, self.region, self.service, "aws4_request"]
            .iter()
            .fold(format!("AWS4{}", self.secret).into_bytes(), |k, part| hmac(&k, part));
        let names: Vec<&str> = r.headers.iter().map(|(n, _)| *n).collect();
        format!(
            "AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={}, Signature={}",
            self.access_key_id,
            names.join(";"),
            hex(&hmac(&key, &to_sign))
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The example AWS publishes for Signature Version 4: IAM's `ListUsers`, signed on
    /// 2015-08-30 with the documentation's example key.
    #[test]
    fn the_published_example_signs_to_the_published_signature() {
        let r = Request {
            method: "GET",
            path: "/",
            query: "Action=ListUsers&Version=2010-05-08",
            headers: &[
                ("content-type", "application/x-www-form-urlencoded; charset=utf-8"),
                ("host", "iam.amazonaws.com"),
                ("x-amz-date", "20150830T123600Z"),
            ],
            body: b"",
        };
        assert_eq!(
            sha256(Signer::canonical(&r).as_bytes()),
            "f536975d06c0309214f805bb90ccff089219ecd68b2577efef23edd43b7e1a59"
        );
        let s = Signer {
            access_key_id: "AKIDEXAMPLE",
            secret: "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY",
            region: "us-east-1",
            service: "iam",
        };
        assert_eq!(
            s.authorization(&r, "20150830T123600Z"),
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20150830/us-east-1/iam/aws4_request, \
             SignedHeaders=content-type;host;x-amz-date, \
             Signature=5d672d79c15b13162d9279b0855cfba6789a8edb4c82c400e06b5924a6f2b5d7"
        );
    }
}
