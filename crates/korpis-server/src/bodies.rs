// SPDX-License-Identifier: AGPL-3.0-or-later

//! The intent body as §3.1 of docs/03-state.md stores it: the `korpis.v1.IntentBody` message in
//! canonical JSON with defaults emitted, through `korpis-wire`. The store is handed this rather
//! than depending on the wire schema itself (§7 of ARCHITECTURE.md).

use korpis_app::ports::{CodecError, IntentCodec};
use korpis_core::workload::IntentBody;
use korpis_wire::body;

/// The stored form of intent bodies.
#[derive(Debug, Clone, Copy, Default)]
pub struct ProtoJsonBodies;

impl IntentCodec for ProtoJsonBodies {
    fn schema_version(&self) -> &str {
        body::SCHEMA_VERSION
    }

    fn encode(&self, b: &IntentBody) -> Result<String, CodecError> {
        body::encode(b).map_err(|e| CodecError(e.to_string()))
    }

    fn decode(&self, json: &str, schema_version: &str) -> Result<IntentBody, CodecError> {
        body::decode(json, schema_version).map_err(|e| CodecError(e.to_string()))
    }
}
