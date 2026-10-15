// SPDX-License-Identifier: AGPL-3.0-or-later

//! The XML the S3 subset answers with: a listing, `ListBucketResult`, and an error.

use crate::store::Page;
use std::fmt::Write as _;
use std::time::{SystemTime, UNIX_EPOCH};

/// `s` with what XML reads as markup escaped.
fn escape(s: &str) -> String {
    s.chars().fold(String::with_capacity(s.len()), |mut out, c| {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c => out.push(c),
        }
        out
    })
}

/// `s` percent-encoded as a URL-encoded listing names keys: every byte but the unreserved and `/`.
pub fn url_encode(s: &str) -> String {
    s.bytes().fold(String::with_capacity(s.len()), |mut out, b| {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~' | b'/') {
            out.push(char::from(b));
        } else {
            let _ = write!(out, "%{b:02X}");
        }
        out
    })
}

/// `t` as ISO 8601 in UTC, to the second.
fn iso8601(t: SystemTime) -> String {
    let secs = t.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let (days, rest) = (secs / 86_400, secs % 86_400);
    let days = i64::try_from(days).unwrap_or(0);
    // The civil date of a day count, by Howard Hinnant's algorithm.
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let of_era = shifted - era * 146_097;
    let year_of = (of_era - of_era / 1460 + of_era / 36_524 - of_era / 146_096) / 365;
    let of_year = of_era - (365 * year_of + year_of / 4 - year_of / 100);
    let from_march = (5 * of_year + 2) / 153;
    let day = of_year - (153 * from_march + 2) / 5 + 1;
    let month = if from_march < 10 { from_march + 3 } else { from_march - 9 };
    let year = year_of + era * 400 + i64::from(month <= 2);
    let (hour, minute, second) = (rest / 3600, rest % 3600 / 60, rest % 60);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.000Z")
}

/// What a listing asked for, as it answers it.
#[derive(Debug)]
pub struct Asked<'a> {
    /// The bucket.
    pub bucket: &'a str,
    /// The prefix.
    pub prefix: &'a str,
    /// The most keys asked for.
    pub max_keys: usize,
    /// Whether keys are URL-encoded.
    pub url_encoded: bool,
}

/// A `ListBucketResult` for `page`; the token of the next page is the last key, in hex.
#[must_use]
pub fn listing(asked: &Asked<'_>, page: &Page) -> String {
    let key = |k: &str| if asked.url_encoded { url_encode(k) } else { escape(k) };
    let mut out = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    out.push_str("<ListBucketResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">");
    let _ = write!(
        out,
        "<Name>{}</Name><Prefix>{}</Prefix><KeyCount>{}</KeyCount><MaxKeys>{}</MaxKeys>\
         <IsTruncated>{}</IsTruncated>",
        escape(asked.bucket),
        key(asked.prefix),
        page.objects.len(),
        asked.max_keys,
        page.more
    );
    if asked.url_encoded {
        out.push_str("<EncodingType>url</EncodingType>");
    }
    for o in &page.objects {
        let _ = write!(
            out,
            "<Contents><Key>{}</Key><LastModified>{}</LastModified><ETag>\"\"</ETag>\
             <Size>{}</Size><StorageClass>STANDARD</StorageClass></Contents>",
            key(&o.key),
            iso8601(o.modified),
            o.size
        );
    }
    if let Some(last) = page.objects.last().filter(|_| page.more) {
        let token = crate::hex(last.key.as_bytes());
        let _ = write!(out, "<NextContinuationToken>{token}</NextContinuationToken>");
    }
    out.push_str("</ListBucketResult>");
    out
}

/// An S3 error document.
#[must_use]
pub fn error(code: &str, message: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<Error><Code>{}</Code><Message>{}</Message></Error>",
        escape(code),
        escape(message)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_date_is_written_as_the_day_it_is() {
        let t = UNIX_EPOCH + std::time::Duration::from_secs(1_440_938_160);
        assert_eq!(iso8601(t), "2015-08-30T12:36:00.000Z");
        assert_eq!(iso8601(UNIX_EPOCH), "1970-01-01T00:00:00.000Z");
    }

    #[test]
    fn keys_are_encoded_as_asked() {
        assert_eq!(url_encode("w/a b+c&d"), "w/a%20b%2Bc%26d");
        assert_eq!(escape("a&<b>"), "a&amp;&lt;b&gt;");
    }
}
