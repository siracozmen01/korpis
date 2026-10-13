// SPDX-License-Identifier: AGPL-3.0-or-later

//! Backup target calls on the wire (§5.6 of docs/06-storage.md).

use crate::timestamp;
use korpis_core::extension::{
    AccessMode, CallOutcome, CallRequest, ObjectAccess, S3Access, StoredObject,
};
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::call_result::Outcome;
use korpis_proto::korpis::v1::__buffa::oneof::object_access::Protocol;
use korpis_proto::korpis::v1::__buffa::oneof::provider_call::Request;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

fn time(t: Option<&buffa_types::google::protobuf::Timestamp>) -> SystemTime {
    t.map_or(UNIX_EPOCH, |t| {
        let secs = u64::try_from(t.seconds).unwrap_or(0);
        UNIX_EPOCH + Duration::new(secs, u32::try_from(t.nanos).unwrap_or(0))
    })
}

/// A backup target request as it goes down a provider's stream, if `r` is one.
#[must_use]
pub fn request_to_pb(r: &CallRequest) -> Option<Request> {
    Some(match r {
        CallRequest::GrantAccess { prefix, mode, ttl } => {
            Request::GrantAccess(Box::new(pb::GrantAccess {
                prefix: prefix.clone(),
                mode: match mode {
                    AccessMode::Write => pb::AccessMode::ACCESS_MODE_WRITE,
                    AccessMode::Read => pb::AccessMode::ACCESS_MODE_READ,
                }
                .into(),
                ttl_seconds: u32::try_from(ttl.as_secs()).unwrap_or(u32::MAX),
                ..Default::default()
            }))
        }
        CallRequest::ListObjects { prefix, after, limit } => {
            Request::ListObjects(Box::new(pb::ListObjects {
                prefix: prefix.clone(),
                after: after.clone(),
                limit: *limit,
                ..Default::default()
            }))
        }
        CallRequest::DeleteObjects { keys } => {
            Request::DeleteObjects(Box::new(pb::DeleteObjects {
                keys: keys.clone(),
                ..Default::default()
            }))
        }
        _ => return None,
    })
}

/// A backup target answer as a provider gave it, if `o` is one; whether it answers its call is
/// the domain's to judge.
#[must_use]
pub fn outcome_from_pb(o: &Outcome) -> Option<CallOutcome> {
    Some(match o {
        Outcome::ObjectAccess(a) => {
            let Some(Protocol::S3(s)) = &a.protocol else {
                return Some(CallOutcome::Failed(korpis_core::extension::CallFailure::malformed()));
            };
            CallOutcome::Access(ObjectAccess {
                expires_at: time(a.expires_at.as_option()),
                s3: S3Access {
                    endpoint: s.endpoint.clone(),
                    region: s.region.clone(),
                    bucket: s.bucket.clone(),
                    key_prefix: s.key_prefix.clone(),
                    access_key_id: s.access_key_id.clone(),
                    secret_access_key: s.secret_access_key.clone(),
                    session_token: s.session_token.clone(),
                },
            })
        }
        Outcome::ObjectList(l) => CallOutcome::Objects {
            objects: l
                .objects
                .iter()
                .map(|o| StoredObject {
                    key: o.key.clone(),
                    size: o.size,
                    modified: time(o.modified.as_option()),
                })
                .collect(),
            more: l.more,
        },
        Outcome::ObjectsDeleted(d) => CallOutcome::Deleted { count: d.deleted },
        _ => return None,
    })
}

/// The wire form of access, for a node.
#[must_use]
pub fn access_to_pb(a: &ObjectAccess) -> pb::ObjectAccess {
    let s = &a.s3;
    pb::ObjectAccess {
        expires_at: timestamp(a.expires_at).into(),
        protocol: Some(Protocol::S3(Box::new(pb::S3Access {
            endpoint: s.endpoint.clone(),
            region: s.region.clone(),
            bucket: s.bucket.clone(),
            key_prefix: s.key_prefix.clone(),
            access_key_id: s.access_key_id.clone(),
            secret_access_key: s.secret_access_key.clone(),
            session_token: s.session_token.clone(),
            ..Default::default()
        }))),
        ..Default::default()
    }
}
