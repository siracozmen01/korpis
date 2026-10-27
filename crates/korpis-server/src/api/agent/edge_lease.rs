// SPDX-License-Identifier: AGPL-3.0-or-later

//! An edge address's lease asked for and released up a member's session (§3.1 of
//! docs/07-networking.md), answered on the same stream with the request's nonce, as a workload's
//! lease is; and what the member's proxy holds of the challenges of its groups' names.

use super::uplink::{Uplink, refusal};
use korpis_app::edge_group::EdgeLeaseAnswer;
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::agent::v1::__buffa::oneof::control_message::Message as Down;
use korpis_wire::duration;
use std::net::IpAddr;

fn answer(request: &agent::EdgeLeaseRequest, a: EdgeLeaseAnswer) -> agent::ControlMessage {
    let (address, nonce) = (request.address.clone(), request.nonce.clone());
    let message = match a {
        EdgeLeaseAnswer::Granted { epoch, term, rho_ppm } => {
            Down::EdgeLeaseGrant(Box::new(agent::EdgeLeaseGrant {
                address,
                nonce,
                epoch,
                term: duration(term).into(),
                rho_ppm,
                ..Default::default()
            }))
        }
        EdgeLeaseAnswer::Refused(r) => {
            let (reason, superseded_by) = refusal(r);
            Down::EdgeLeaseRefusal(Box::new(agent::EdgeLeaseRefusal {
                address,
                nonce,
                reason: reason.into(),
                superseded_by,
                ..Default::default()
            }))
        }
    };
    agent::ControlMessage { message: Some(message), ..Default::default() }
}

fn address(text: &str) -> Result<IpAddr, String> {
    text.parse().map_err(|_| format!("{text:?} is not an address"))
}

/// Decides the member's request and answers it.
pub(super) async fn request(up: &Uplink, r: &agent::EdgeLeaseRequest) -> Result<(), String> {
    let at = address(&r.address)?;
    let a = up.app.request_edge_lease(up.node, at).await.map_err(|e| e.to_string())?;
    up.tx.send(answer(r, a)).await.map_err(|e| e.to_string())
}

/// Records which of its groups' challenges the member's proxy holds.
pub(super) async fn challenges(up: &Uplink, h: &agent::EdgeChallengesHeld) -> Result<(), String> {
    let held = korpis_app::ports::ChallengesHeld {
        tokens: h.tokens.clone(),
        digests: h.alpn_digests.clone(),
    };
    up.app.record_edge_challenges(up.node, &held).await.map_err(|e| e.to_string())
}

/// Records that the member stopped holding the address.
pub(super) async fn released(up: &Uplink, r: &agent::EdgeLeaseReleased) -> Result<(), String> {
    let at = address(&r.address)?;
    up.app.release_edge_lease(up.node, at, r.epoch).await.map_err(|e| e.to_string())?;
    Ok(())
}
