// SPDX-License-Identifier: AGPL-3.0-or-later

//! DNS records as Korpis publishes them through a DNS provider (§3.5 of
//! docs/07-networking.md). A record is checked here, before any provider sees it, so a provider
//! is never asked to publish something malformed and every provider is asked the same thing.

use std::net::{Ipv4Addr, Ipv6Addr};

/// The most records one owner may have.
pub const MAX_RECORDS: usize = 100;

/// The shortest TTL accepted, in seconds.
pub const MIN_TTL: u32 = 30;

/// The longest TTL accepted, in seconds.
pub const MAX_TTL: u32 = 86_400;

/// The longest owner key, in bytes.
pub const MAX_OWNER: usize = 128;

/// The kinds of record published.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RecordType {
    /// An IPv4 address.
    A,
    /// An IPv6 address.
    Aaaa,
    /// Another name.
    Cname,
    /// A service location: priority, weight, port, target.
    Srv,
    /// Text.
    Txt,
}

impl RecordType {
    /// Its name in documents and in DNS.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::A => "A",
            Self::Aaaa => "AAAA",
            Self::Cname => "CNAME",
            Self::Srv => "SRV",
            Self::Txt => "TXT",
        }
    }
}

/// One record.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DnsRecord {
    /// A fully qualified name, lowercase, without the trailing dot.
    pub name: String,
    /// Its type.
    pub kind: RecordType,
    /// Seconds.
    pub ttl: u32,
    /// As the type writes them.
    pub values: Vec<String>,
}

/// Why a record cannot be published.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{field}: {message}")]
pub struct RecordError {
    /// Which part, e.g. `records[2].values[0]`.
    pub field: String,
    /// Why.
    pub message: String,
}

fn err(field: impl Into<String>, message: impl Into<String>) -> RecordError {
    RecordError { field: field.into(), message: message.into() }
}

/// Whether `name` is a hostname: dotted labels of letters, digits, and inner hyphens, lowercase,
/// a leading `_` allowed for service labels (`_minecraft._tcp`), at most 253 bytes.
fn hostname(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 253
        && name.split('.').all(|label| {
            let body = label.strip_prefix('_').unwrap_or(label);
            !body.is_empty()
                && label.len() <= 63
                && !body.starts_with('-')
                && !body.ends_with('-')
                && body.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        })
}

fn value(kind: RecordType, v: &str) -> Result<(), &'static str> {
    let ok = match kind {
        RecordType::A => v.parse::<Ipv4Addr>().is_ok(),
        RecordType::Aaaa => v.parse::<Ipv6Addr>().is_ok(),
        RecordType::Cname => hostname(v),
        RecordType::Srv => {
            let parts: Vec<&str> = v.split(' ').collect();
            matches!(parts.as_slice(), [p, w, port, target]
                if p.parse::<u16>().is_ok() && w.parse::<u16>().is_ok()
                    && port.parse::<u16>().is_ok_and(|n| n > 0) && hostname(target))
        }
        RecordType::Txt => v.len() <= 255 && !v.chars().any(char::is_control),
    };
    if ok {
        return Ok(());
    }
    Err(match kind {
        RecordType::A => "an IPv4 address",
        RecordType::Aaaa => "an IPv6 address",
        RecordType::Cname => "a hostname",
        RecordType::Srv => "\"priority weight port target\"",
        RecordType::Txt => "text of at most 255 bytes without control characters",
    })
}

impl DnsRecord {
    /// Checks one record, naming its fields under `at`.
    pub fn check(&self, at: &str) -> Result<(), RecordError> {
        if !hostname(&self.name) {
            return Err(err(format!("{at}.name"), "a lowercase hostname, without the final dot"));
        }
        if !(MIN_TTL..=MAX_TTL).contains(&self.ttl) {
            return Err(err(format!("{at}.ttl"), format!("{MIN_TTL} to {MAX_TTL} seconds")));
        }
        if self.values.is_empty() {
            return Err(err(format!("{at}.values"), "at least one"));
        }
        if self.kind == RecordType::Cname && self.values.len() > 1 {
            return Err(err(format!("{at}.values"), "a CNAME has exactly one"));
        }
        for (i, v) in self.values.iter().enumerate() {
            value(self.kind, v).map_err(|why| err(format!("{at}.values[{i}]"), why))?;
        }
        Ok(())
    }
}

/// Checks an owner's records: its key, how many, each record, no name both a CNAME and anything
/// else (DNS forbids it), and no record twice.
pub fn check_records(owner: &str, records: &[DnsRecord]) -> Result<(), RecordError> {
    if owner.is_empty() || owner.len() > MAX_OWNER || owner.chars().any(char::is_control) {
        return Err(err("owner", format!("an identifier of at most {MAX_OWNER} bytes")));
    }
    if records.len() > MAX_RECORDS {
        return Err(err("records", format!("at most {MAX_RECORDS}")));
    }
    for (i, r) in records.iter().enumerate() {
        r.check(&format!("records[{i}]"))?;
        let clash = records.iter().take(i).any(|o| {
            o.name == r.name
                && (o.kind == r.kind || o.kind == RecordType::Cname || r.kind == RecordType::Cname)
        });
        if clash {
            let why = "a name has one record set per type, and a CNAME nothing beside it";
            return Err(err(format!("records[{i}].name"), why));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(name: &str, kind: RecordType, values: &[&str]) -> DnsRecord {
        DnsRecord {
            name: name.into(),
            kind,
            ttl: 300,
            values: values.iter().map(|v| (*v).to_owned()).collect(),
        }
    }

    #[test]
    fn well_formed_records_of_every_type_pass() {
        let ok = [
            record("play.example.com", RecordType::A, &["203.0.113.7"]),
            record("play.example.com", RecordType::Aaaa, &["2001:db8::7"]),
            record("www.example.com", RecordType::Cname, &["web.example.com"]),
            record(
                "_minecraft._tcp.play.example.com",
                RecordType::Srv,
                &["0 5 25565 n1.example.com"],
            ),
            record("_acme-challenge.example.com", RecordType::Txt, &["token-value"]),
        ];
        assert_eq!(check_records("ep_1", &ok), Ok(()));
    }

    #[test]
    fn a_malformed_record_is_refused_naming_the_field() {
        let field = |records: &[DnsRecord]| check_records("ep_1", records).unwrap_err().field;
        assert_eq!(
            field(&[record("Play.example.com", RecordType::A, &["1.2.3.4"])]),
            "records[0].name"
        );
        assert_eq!(
            field(&[record("a.example.com", RecordType::A, &["::1"])]),
            "records[0].values[0]"
        );
        assert_eq!(
            field(&[record("a.example.com", RecordType::Srv, &["0 5 0 x.com"])]),
            "records[0].values[0]"
        );
        let mut short = record("a.example.com", RecordType::A, &["1.2.3.4"]);
        short.ttl = 5;
        assert_eq!(field(&[short]), "records[0].ttl");
        let clash = [
            record("a.example.com", RecordType::Cname, &["b.example.com"]),
            record("a.example.com", RecordType::A, &["1.2.3.4"]),
        ];
        assert_eq!(field(&clash), "records[1].name");
        assert_eq!(check_records("", &[]).unwrap_err().field, "owner");
    }
}
