// SPDX-License-Identifier: AGPL-3.0-or-later

//! What an install is given (§4 and §6 of docs/09-recipes.md), for the documented example: the
//! template verb's format, every target inside a volume, contexts built from declared values with
//! no secret in reach, and which templates a change of values renders again.

use crate::support::PAPER;
use korpis_core::recipe::install::InstallStep;
use korpis_core::recipe::installing::{InstallReason, actions_of, context};
use korpis_core::recipe::template::{TemplateFormat, Var};
use korpis_core::value::{Value, ValueMap};
use korpis_core::workload::spec::VolumeSpec;
use korpis_core::workload::spec::volumes::VolumeMode;
use korpis_recipe::parse;

fn volume(path: &str) -> VolumeSpec {
    VolumeSpec {
        name: "data".into(),
        path: path.into(),
        size_bytes: 1 << 30,
        storage_class: String::new(),
        mode: VolumeMode::ReadWrite,
        existing: None,
        from_backup: None,
    }
}

#[test]
fn a_template_verb_says_its_format_and_installs_only_into_volumes() {
    let step = r#"    - write: {path: /data/eula.txt, content: "eula=true\n"}"#;
    let verb = r"    - template: {from: templates/server.properties, to: /data/start.env, format: env, vars: [motd]}";
    let doc = PAPER.replacen(step, &format!("{step}\n{verb}"), 1);
    let r = parse(doc.as_bytes()).unwrap();
    let InstallStep::Template { format, .. } = &r.install[3] else { panic!("{:?}", r.install[3]) };
    assert_eq!(*format, TemplateFormat::Env);
    let without = doc.replacen(", format: env", "", 1);
    assert!(parse(without.as_bytes()).is_err(), "a template verb names its format");

    r.check_targets(&[volume("/data")]).unwrap();
    let e = r.check_targets(&[volume("/srv")]).unwrap_err();
    assert_eq!(e.field, "install.steps[0].to", "the first target outside: {e:?}");
}

#[test]
fn a_context_holds_declared_values_as_they_are_written_and_never_a_secret() {
    let r = parse(PAPER.as_bytes()).unwrap();
    let values = ValueMap::from([
        ("motd".to_owned(), Value::String("Hi".into())),
        ("max_players".to_owned(), Value::Int(20)),
        ("rcon_password".to_owned(), Value::String("hunter2".into())),
    ]);
    let vars = ["motd", "max_players", "difficulty"].map(str::to_owned);
    let c = context(&r.config, &values, &vars).unwrap();
    assert_eq!(c["motd"], Var::Text("Hi".into()));
    assert_eq!(c["max_players"], Var::Number("20".into()));
    assert_eq!(c["difficulty"], Var::Unset, "no value declared");
    assert!(context(&r.config, &values, &["rcon_password".to_owned()]).is_err(), "a secret");

    let detail = r.install_detail(&values, InstallReason::Create, (true, &[0])).unwrap();
    let actions = actions_of(&detail).unwrap();
    assert_eq!(actions.len(), r.install.len() + 1, "every verb, then the template");
    for (kept, step) in actions.iter().zip(&r.install) {
        assert_eq!(&kept.step, step, "read back as declared");
    }
    let rendered = actions.last().unwrap();
    assert!(matches!(rendered.step, InstallStep::Template { .. }));
    assert_eq!(rendered.context.as_ref(), Some(&c), "with its context");
    assert!(!format!("{detail:?}").contains("hunter2"), "no secret in a plan's detail");
}

#[test]
fn a_change_of_values_renders_again_only_the_templates_that_use_them() {
    let r = parse(PAPER.as_bytes()).unwrap();
    assert_eq!(r.templates_using(&["motd".to_owned()]), [0]);
    assert!(r.templates_using(&["rcon_password".to_owned()]).is_empty());
    assert!(r.installs());
}
