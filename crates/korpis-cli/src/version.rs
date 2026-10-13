// SPDX-License-Identifier: AGPL-3.0-or-later

//! `korpis version`: the client's version and source revision, and the control plane's, each
//! with where its source is published (§8 of docs/13-surface-cli.md), which is how a conveyed
//! copy meets the AGPL without anyone doing anything (§2 of docs/19-governance.md).

use crate::call::call;
use crate::connect::Connection;
use crate::error::CliError;
use serde_json::{Value, json};

/// The client's own version, as JSON.
#[must_use]
pub fn client() -> Value {
    json!({
        "version": korpis_core::build::VERSION,
        "sourceRevision": korpis_core::build::SOURCE_REVISION,
        "sourceUrl": korpis_core::build::SOURCE_URL,
    })
}

/// The client's version and the server's, as JSON; the server's is absent without a
/// connection.
pub async fn both(c: Option<&Connection>) -> Result<Value, CliError> {
    let mut server = Value::Null;
    if let Some(c) = c {
        call(c, "ServerInfoService/GetServerInfo", json!({}), &mut |v| server = v).await?;
    }
    Ok(json!({ "client": client(), "server": server }))
}

/// The human form of [`both`].
#[must_use]
pub fn human(v: &Value) -> String {
    let line = |who: &str, x: &Value| {
        let field = |k: &str| x.get(k).and_then(Value::as_str).unwrap_or("unknown").to_owned();
        format!(
            "{who:<7}{} (source {} at {})",
            field("version"),
            field("sourceUrl"),
            field("sourceRevision")
        )
    };
    let mut out = line("client", &v["client"]);
    if !v["server"].is_null() {
        out.push('\n');
        out.push_str(&line("server", &v["server"]));
    }
    out
}
