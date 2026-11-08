// SPDX-License-Identifier: AGPL-3.0-or-later

//! `korpis extension invoke <ext_…> [<action>] [--arg name=value]…`: the commands an extension adds
//! are generated from what its document declares. Without an action it lists the actions and
//! their arguments; with one it reads each argument as its declared type (a workload or project by
//! name or identifier) and invokes it, printing what the extension answered.

use super::{Answer, json, pairs};
use crate::connect::Connection;
use crate::error::CliError;
use crate::resolve;
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::action_value::Value as V;
use std::collections::BTreeMap;
use std::fmt::Write as _;

fn listed(name: &str, actions: &[pb::ActionDecl]) -> String {
    if actions.is_empty() {
        return format!("{name} declares no actions\n");
    }
    let mut out = String::new();
    for a in actions {
        let _ = writeln!(out, "ext.{name}.{}  {}", a.name, a.description);
        for arg in &a.arguments {
            let need = if arg.required { "required" } else { "optional" };
            let _ = writeln!(out, "    --arg {}=<{}>  {need}", arg.name, arg.r#type);
        }
    }
    out
}

async fn value(c: &Connection, arg: &pb::ActionArgument, given: &str) -> Result<V, CliError> {
    let bad = |what: &str| CliError::Local(format!("--arg {}: {what}", arg.name));
    Ok(match arg.r#type.as_str() {
        "string" => V::Text(given.to_owned()),
        "int" => V::Int(given.parse().map_err(|_| bad("a whole number"))?),
        "bool" => V::Flag(given.parse().map_err(|_| bad("true or false"))?),
        "workload" => V::Workload(resolve::workload(c, given).await?),
        "project" => V::Project(resolve::project(c, given).await?),
        other => return Err(bad(&format!("of a type this CLI does not know, {other}"))),
    })
}

async fn arguments(
    c: &Connection,
    declared: &pb::ActionDecl,
    given: &[String],
) -> Result<BTreeMap<String, pb::ActionValue>, CliError> {
    let mut out = BTreeMap::new();
    for (k, v) in pairs("--arg", given)? {
        let Some(arg) = declared.arguments.iter().find(|a| a.name == k) else {
            return Err(CliError::Local(format!(
                "--arg {k}: not an argument of {}",
                declared.name
            )));
        };
        let value = Some(value(c, arg, &v).await?);
        out.insert(k, pb::ActionValue { value, ..Default::default() });
    }
    Ok(out)
}

/// `korpis extension invoke`.
pub async fn invoke(
    c: &Connection,
    extension: &str,
    action: Option<&str>,
    given: &[String],
) -> Result<Answer, CliError> {
    let client = pb::ExtensionServiceClient::new(c.http(), c.config());
    let request = pb::GetExtensionRequest { id: extension.to_owned(), ..Default::default() };
    let e = client.get_extension(request).await?.into_owned().extension.into_option();
    let e = e.unwrap_or_default();
    let actions = e.manifest.as_option().map(|m| m.actions.clone()).unwrap_or_default();
    let Some(action) = action else {
        return Ok(Answer { human: listed(&e.name, &actions), json: json(&actions) });
    };
    let declared = actions.iter().find(|a| a.name == action).ok_or_else(|| {
        CliError::Local(format!("{} declares no action {action}; run without it to list", e.name))
    })?;
    let request = pb::InvokeActionRequest {
        extension: extension.to_owned(),
        action: action.to_owned(),
        arguments: arguments(c, declared, given).await?,
        ..Default::default()
    };
    let r = client.invoke_action(request).await?.into_owned();
    if let Some(err) = r.error.as_option() {
        let why = format!("ext.{}.{action} failed: {}: {}", e.name, err.reason, err.message);
        return Err(CliError::Local(why));
    }
    let result = r.result.as_option().cloned().unwrap_or_default();
    let mut human = result.message.clone();
    if !human.ends_with('\n') {
        human.push('\n');
    }
    if !result.data.is_empty() {
        let _ = writeln!(human, "{}", result.data);
    }
    Ok(Answer { human, json: json(&r) })
}
