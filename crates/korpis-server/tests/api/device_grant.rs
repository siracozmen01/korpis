// SPDX-License-Identifier: AGPL-3.0-or-later

//! The CLI's sign-in over the API (§9.4 of docs/08-identity.md, RFC 8628): a device authorization
//! begun without a credential and pointed at the panel's device page, pending until a signed-in
//! user approves its code, then answered with a CLI session whose refresh credential exchanges
//! in the body.

use crate::panel::{PANEL, access, browser, invite, redeem_keeping_cookie};
use crate::refusal::{detail, reason};
use crate::support::Harness;
use connectrpc::client::HttpClient;
use korpis_proto::korpis::v1 as pb;

fn sessions(h: &Harness, bearer: Option<&str>) -> pb::SessionServiceClient<HttpClient> {
    pb::SessionServiceClient::new(h.http.clone(), h.config(bearer))
}

async fn authorize(h: &Harness) -> pb::AuthorizeDeviceResponse {
    let begun = sessions(h, None).authorize_device(pb::AuthorizeDeviceRequest::default()).await;
    begun.unwrap().into_owned()
}

fn token(a: &pb::AuthorizeDeviceResponse) -> pb::DeviceTokenRequest {
    pb::DeviceTokenRequest { device_code: a.device_code.clone(), ..Default::default() }
}

#[tokio::test]
async fn a_cli_signs_in_once_its_user_code_is_approved_in_the_panel() {
    let h = Harness::new().await;
    let invitation = invite(&h, "invite-friend", "A Friend").await.invitation;
    let (_, cookie) = redeem_keeping_cookie(&h, &invitation, "friend", &browser()).await.unwrap();
    let panel = sessions(&h, Some(&access(&h, &cookie).await));

    let waiting = authorize(&h).await;
    assert_eq!(waiting.verification_uri, format!("{PANEL}/device"));
    assert_eq!(waiting.interval, 5);
    let pending = sessions(&h, None).device_token(token(&waiting)).await.unwrap_err();
    assert_eq!(reason(&pending), "device.pending");
    assert!(detail(&pending).retryable);

    let a = authorize(&h).await;
    let code = a.user_code.to_lowercase();
    let shown = panel
        .get_device_authorization(pb::GetDeviceAuthorizationRequest {
            user_code: code.clone(),
            ..Default::default()
        })
        .await
        .unwrap()
        .into_owned();
    assert_eq!(shown.user_code, a.user_code);
    assert!(shown.source.starts_with("127.0.0.1"), "{}", shown.source);
    let approve = pb::ApproveDeviceRequest { user_code: code, ..Default::default() };
    panel.approve_device(approve).await.unwrap();

    let signed_in = sessions(&h, None).device_token(token(&a)).await.unwrap().into_owned();
    assert!(signed_in.refresh.starts_with("korpisrt_"));
    let request = pb::ExchangeRequest { refresh: signed_in.refresh, ..Default::default() };
    let exchanged = sessions(&h, None).exchange(request).await.unwrap().into_owned();
    let cli = sessions(&h, Some(&exchanged.access_token));
    let listed = cli.list_sessions(pb::ListSessionsRequest::default()).await.unwrap();
    let listed = listed.into_owned().sessions;
    let mine = listed.iter().find(|s| s.current).unwrap();
    assert_eq!(mine.kind, "cli");
    assert_eq!(h.effects("device.approved").await.len(), 1);
}
