// SPDX-License-Identifier: AGPL-3.0-or-later

//! DNS records' wire form, `korpis.v1.DnsRecord` (§3.5 of docs/07-networking.md).

use crate::{Result, invalid};
use korpis_core::dns::{DnsRecord, RecordType};
use korpis_proto::korpis::v1 as pb;

enum_map!(record_type_to_pb, record_type_from_pb, RecordType, pb::DnsRecordType, "a record type", {
    RecordType::A => pb::DnsRecordType::DNS_RECORD_TYPE_A,
    RecordType::Aaaa => pb::DnsRecordType::DNS_RECORD_TYPE_AAAA,
    RecordType::Cname => pb::DnsRecordType::DNS_RECORD_TYPE_CNAME,
    RecordType::Srv => pb::DnsRecordType::DNS_RECORD_TYPE_SRV,
    RecordType::Txt => pb::DnsRecordType::DNS_RECORD_TYPE_TXT,
});

/// The wire form of a record.
#[must_use]
pub fn record_to_pb(r: &DnsRecord) -> pb::DnsRecord {
    pb::DnsRecord {
        name: r.name.clone(),
        r#type: record_type_to_pb(r.kind).into(),
        ttl: r.ttl,
        values: r.values.clone(),
        ..Default::default()
    }
}

/// A record from the field `field`; its contents are checked by the domain.
pub fn record_from_pb(field: &str, r: &pb::DnsRecord) -> Result<DnsRecord> {
    if r.name.is_empty() {
        return Err(invalid(&format!("{field}.name"), "required"));
    }
    Ok(DnsRecord {
        name: r.name.clone(),
        kind: record_type_from_pb(&format!("{field}.type"), r.r#type)?,
        ttl: r.ttl,
        values: r.values.clone(),
    })
}
