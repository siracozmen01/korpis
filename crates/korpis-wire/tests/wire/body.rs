// SPDX-License-Identifier: AGPL-3.0-or-later

//! The stored intent body (§3.1 of docs/03-state.md).

use crate::support::body;
use korpis_wire::body::{SCHEMA_VERSION, decode, encode};
use serde_json::Value;

#[test]
fn a_body_reads_back_exactly() {
    let b = body();
    let stored = encode(&b).unwrap();
    assert_eq!(decode(&stored, SCHEMA_VERSION).unwrap(), b);
}

#[test]
fn defaults_are_written_so_a_later_default_cannot_change_an_old_row() {
    let json: Value = serde_json::from_str(&encode(&body()).unwrap()).unwrap();
    // Implicit-presence fields left at their default are present, in canonical form.
    assert_eq!(json["schedule"], "");
    assert_eq!(json["resources"]["disk"]["reservation"], "0", "64-bit integers are strings");
    assert_eq!(json["resources"]["io"]["share"], 0);
    assert_eq!(json["endpoints"][0]["domains"], Value::Array(Vec::new()));
    assert_eq!(json["endpoints"][0]["accounting"], false);
    assert_eq!(
        json["endpoints"][1]["policy"]["ingress"][0]["protocol"],
        "ENDPOINT_PROTOCOL_UNSPECIFIED"
    );
    assert_eq!(json["volumes"][0]["mode"], "VOLUME_MODE_RW");
    // Absence that means "not set" stays absent: an endpoint without TLS, a volume created
    // rather than attached, a probe without a timeout.
    assert!(json["endpoints"][0].get("tls").is_none());
    assert!(json["volumes"][0].get("existing").is_none());
    assert!(json["health"]["readiness"].get("timeout").is_none());
}

#[test]
fn encoding_is_deterministic() {
    let b = body();
    assert_eq!(encode(&b).unwrap(), encode(&b).unwrap());
}

#[test]
fn a_body_from_another_schema_is_refused_not_guessed() {
    let stored = encode(&body()).unwrap();
    let e = decode(&stored, "korpis.v2").unwrap_err();
    assert_eq!(e.field, "schema_version");
    let e = decode(r#"{"lifecycle":"LIFECYCLE_PERSISTENT"}"#, SCHEMA_VERSION).unwrap_err();
    assert_eq!(e.field, "body.recipe");
}
