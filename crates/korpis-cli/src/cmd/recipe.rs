// SPDX-License-Identifier: AGPL-3.0-or-later

//! `korpis workload recipe`: the recipe a workload runs, as the panel shows it (§5 and §5.3 of
//! docs/09-recipes.md), in the caller's language where the recipe speaks it.

use super::{Answer, json};
use crate::connect::Connection;
use crate::error::CliError;
use crate::show::{short, table};
use korpis_core::recipe::messages::{DESCRIPTION, label_key, plain};
use korpis_proto::korpis::v1 as pb;

/// The language the environment names, as a BCP 47 tag: `LC_ALL`, else `LC_MESSAGES`, else
/// `LANG`, with its encoding and modifier dropped (`tr_TR.UTF-8` is `tr-TR`). The C locale names
/// none.
fn environment_locale(read: impl Fn(&str) -> Option<String>) -> Option<String> {
    let set = ["LC_ALL", "LC_MESSAGES", "LANG"]
        .into_iter()
        .find_map(|v| read(v).filter(|s| !s.is_empty()))?;
    let tag = set.split(['.', '@']).next().unwrap_or_default().replace('_', "-");
    (tag != "C" && tag != "POSIX" && !tag.is_empty()).then_some(tag)
}

fn value(v: Option<&pb::Value>) -> String {
    v.map_or_else(|| "-".to_owned(), |v| serde_json::to_string(&json(v)).unwrap_or_default())
}

fn marks(f: &pb::ConfigField) -> String {
    let marks = [
        (f.secret, "secret"),
        (f.hidden, "hidden"),
        (!f.derived.is_empty(), "derived"),
        (f.r#const.is_set(), "fixed"),
    ];
    let said: Vec<&str> = marks.iter().filter(|(on, _)| *on).map(|(_, m)| *m).collect();
    if said.is_empty() { "-".to_owned() } else { said.join(",") }
}

/// Answers `korpis workload recipe` for workload `id`.
pub async fn recipe(c: &Connection, id: String, asked: &[String]) -> Result<Answer, CliError> {
    let locales = if asked.is_empty() {
        environment_locale(|v| std::env::var(v).ok()).into_iter().collect()
    } else {
        asked.to_vec()
    };
    let request = pb::GetWorkloadRecipeRequest { workload: id, locales, ..Default::default() };
    let client = pb::WorkloadServiceClient::new(c.http(), c.config());
    let r = client.get_workload_recipe(request).await?.into_owned();
    let said = |key: &str| r.messages.get(key).map(|m| plain(m));
    let reference = r.recipe.as_option().cloned().unwrap_or_default();
    let mut human = format!(
        "{} {} ({}), in {}\n",
        reference.name, reference.version, reference.digest, r.locale
    );
    if let Some(d) = said(DESCRIPTION) {
        human.push_str(&d);
        human.push('\n');
    }
    let mut rows = vec![
        ["KEY", "TYPE", "WHO MAY SET", "LABEL", "DEFAULT", "MARKS"].map(str::to_owned).to_vec(),
    ];
    for f in &r.config.as_option().cloned().unwrap_or_default().fields {
        rows.push(vec![
            f.key.clone(),
            short(f.r#type, "CONFIG_TYPE_"),
            short(f.permission, "FIELD_PERMISSION_"),
            said(&label_key(&f.key)).unwrap_or_else(|| "-".to_owned()),
            value(f.default.as_option()),
            marks(f),
        ]);
    }
    if rows.len() > 1 {
        human.push('\n');
        human.push_str(&table(&rows));
    }
    Ok(Answer { json: json(&r), human })
}

#[cfg(test)]
mod tests {
    use super::environment_locale;

    #[test]
    fn the_environment_s_language_is_a_tag() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |v: &str| pairs.iter().find(|(k, _)| *k == v).map(|(_, x)| (*x).to_owned())
        };
        assert_eq!(environment_locale(env(&[("LANG", "tr_TR.UTF-8")])), Some("tr-TR".into()));
        assert_eq!(
            environment_locale(env(&[("LANG", "en_US.UTF-8"), ("LC_ALL", "pt_BR")])),
            Some("pt-BR".into())
        );
        assert_eq!(environment_locale(env(&[("LANG", "C.UTF-8")])), None);
        assert_eq!(environment_locale(env(&[])), None);
    }
}
