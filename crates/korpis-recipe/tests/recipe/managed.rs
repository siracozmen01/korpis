// SPDX-License-Identifier: AGPL-3.0-or-later

//! Managed files (§5.2 of docs/09-recipes.md): an operator's setting, a secret among them,
//! rendered outside every volume the tenant writes, from a template that may name only the fields
//! delivered that way, and no such field left with nowhere to go.

use crate::support::PAPER;
use korpis_core::recipe::managed::{MANAGED_DIR, context};
use korpis_core::recipe::template::{TemplateFormat, Var};
use korpis_core::value::{Value, ValueMap};
use korpis_recipe::{DocumentError, parse};

const FIELD: &str = "    - {key: eula, type: boolean, const: true, hidden: true}";

/// The example with `fields` added to its config and `managed` as its managed files.
fn with(fields: &str, managed: &str) -> String {
    let doc = PAPER.replacen(FIELD, &format!("{FIELD}\n{fields}"), 1);
    if managed.is_empty() { doc } else { format!("{doc}\nmanaged_files:\n{managed}") }
}

const LIMITS: &str = "    - {key: max_view, type: integer, min: 2, max: 32, default: 10, \
                      permission: operator, deliver: managed_file}\n    \
                      - {key: rcon_token, type: string, secret: true, permission: operator, \
                      deliver: managed_file, pattern: '[^\\r\\n]*'}";

fn refused(doc: &str) -> String {
    match parse(doc.as_bytes()) {
        Err(DocumentError::Invalid(e)) => e.field,
        other => panic!("{other:?}"),
    }
}

#[test]
fn an_operator_field_and_a_secret_reach_a_file_the_tenant_cannot_write() {
    let managed = "  - {name: limits.properties, from: managed/limits.properties, \
                   format: properties, vars: [max_view, rcon_token]}";
    let r = parse(with(LIMITS, managed).as_bytes()).unwrap();
    let file = &r.managed_files[0];
    assert_eq!(file.path(), format!("{MANAGED_DIR}/limits.properties"));
    assert_eq!(file.format, TemplateFormat::Properties);
    assert!(r.artifact_paths().contains(&"managed/limits.properties"), "the artifact holds it");
    let values = ValueMap::from([
        ("max_view".to_owned(), Value::Int(16)),
        ("rcon_token".to_owned(), Value::String("s3cret".into())),
    ]);
    let c = context(&r.config, &values, file).unwrap();
    assert_eq!(c.get("rcon_token"), Some(&Var::Text("s3cret".into())), "a secret is in scope here");
    assert_eq!(c.get("max_view"), Some(&Var::Number("16".into())));
}

#[test]
fn a_managed_file_reaches_only_its_own_fields_and_every_such_field_reaches_one() {
    let file = |vars: &str| {
        format!("  - {{name: l.env, from: managed/l.env, format: env, vars: [{vars}]}}")
    };
    assert_eq!(
        refused(&with(LIMITS, &file("max_view, motd"))),
        "managed_files[0].vars",
        "motd is a template's"
    );
    assert_eq!(
        refused(&with(LIMITS, &file("max_view"))),
        "managed_files",
        "rcon_token has nowhere to go"
    );
    assert_eq!(refused(&with(LIMITS, "")), "managed_files", "nor has either without a file");
    let two = format!("{}\n{}", file("max_view, rcon_token"), file("max_view"));
    assert_eq!(refused(&with(LIMITS, &two)), "managed_files[1].name", "a name once");
    let named = |name: &str| {
        format!(
            "  - {{name: '{name}', from: managed/l.env, format: env, vars: [max_view, rcon_token]}}"
        )
    };
    for bad in ["../etc/passwd", "a/b", "..", ""] {
        assert_eq!(refused(&with(LIMITS, &named(bad))), "managed_files[0].name", "{bad:?}");
    }
    let template = "templates:\n  - {from: templates/server.properties, path: /data/x, \
                    format: properties, vars: [max_view]}\n";
    let leaked = with(LIMITS, &file("max_view, rcon_token")).replacen("templates:\n", template, 1);
    assert!(parse(leaked.as_bytes()).is_err(), "a volume's template never names a managed field");
}
