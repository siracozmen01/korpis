// SPDX-License-Identifier: AGPL-3.0-or-later

//! `korpis sftp <workload>` (§8 of docs/13-surface-cli.md): where an SFTP client reaches a
//! workload's files, and which host key to expect, for each server `ServerInfo` names. It opens
//! nothing itself, the client being the person's own; it prints the line a client takes, the
//! fingerprint a client shows on its first connection, and a `known_hosts` line to add, so that
//! connection is checked rather than trusted.

use super::{Answer, json};
use crate::connect::Connection;
use crate::error::CliError;
use crate::resolve;
use korpis_proto::korpis::v1 as pb;

/// What a person types, and adds, to reach `workload` at `s`.
fn lines(workload: &str, s: &pb::SftpServer) -> String {
    let (host, port) = s.address.rsplit_once(':').unwrap_or((s.address.as_str(), "22"));
    let at = if port == "22" { String::new() } else { format!("-P {port} ") };
    let by = if s.relay.is_empty() { "the control plane".to_owned() } else { s.relay.clone() };
    format!(
        "sftp {at}{workload}@{host}\n  served by {by}\n  host key {}\n  known_hosts: {} {}",
        s.fingerprint, s.known_hosts_name, s.host_key
    )
}

/// Runs `korpis sftp <named>`.
///
/// # Errors
/// The workload is not found, or the control plane could not be asked.
pub async fn sftp(c: &Connection, named: &str) -> Result<Answer, CliError> {
    let workload = resolve::workload(c, named).await?;
    let info = pb::ServerInfoServiceClient::new(c.http(), c.config())
        .get_server_info(pb::GetServerInfoRequest::default())
        .await?
        .into_owned();
    let human = if info.sftp.is_empty() {
        "no relay serves SFTP here; an operator turns it on with --sftp-listen".to_owned()
    } else {
        info.sftp.iter().map(|s| lines(&workload, s)).collect::<Vec<_>>().join("\n")
    };
    let listed = pb::GetServerInfoResponse { sftp: info.sftp, ..Default::default() };
    Ok(Answer { json: json(&listed), human })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_server_is_printed_as_a_client_takes_it_and_known_hosts_names_it() {
        let s = pb::SftpServer {
            address: "relay.example:2022".into(),
            known_hosts_name: "[relay.example]:2022".into(),
            host_key: "ssh-ed25519 AAAA".into(),
            fingerprint: "SHA256:abc".into(),
            relay: "rly_1".into(),
            ..Default::default()
        };
        let printed = lines("wl_1", &s);
        assert!(printed.starts_with("sftp -P 2022 wl_1@relay.example\n"), "{printed}");
        assert!(printed.contains("known_hosts: [relay.example]:2022 ssh-ed25519 AAAA"));
        let v6 = pb::SftpServer { address: "[2001:db8::1]:22".into(), ..s };
        assert!(lines("wl_1", &v6).starts_with("sftp wl_1@[2001:db8::1]\n"));
    }
}
