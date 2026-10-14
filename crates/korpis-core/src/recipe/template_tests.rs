// SPDX-License-Identifier: AGPL-3.0-or-later

use super::*;

fn ctx(pairs: &[(&str, Var)]) -> Context {
    pairs.iter().map(|(k, v)| ((*k).to_owned(), v.clone())).collect()
}

fn text(s: &str) -> Var {
    Var::Text(s.to_owned())
}

#[test]
fn values_are_substituted_and_sections_test_them() {
    let t = Template::parse(
        "motd={{motd}}\n{{#pvp}}pvp=on\n{{/pvp}}{{^pvp}}pvp=off\n{{/pvp}}{{! a note }}max={{ max }}",
    )
    .unwrap();
    assert_eq!(t.names(), ["max", "motd", "pvp"].map(str::to_owned).into());
    let c =
        ctx(&[("motd", text("Hi")), ("pvp", Var::Bool(false)), ("max", Var::Number("20".into()))]);
    assert_eq!(t.render(TemplateFormat::Ini, &c).unwrap(), "motd=Hi\npvp=off\nmax=20");
    let c = ctx(&[("motd", text("Hi")), ("pvp", Var::Bool(true)), ("max", Var::Unset)]);
    assert_eq!(t.render(TemplateFormat::Ini, &c).unwrap(), "motd=Hi\npvp=on\nmax=");
}

#[test]
fn nothing_outside_the_context_is_reachable_and_malformed_tags_refuse() {
    let t = Template::parse("{{token}}").unwrap();
    assert!(t.render(TemplateFormat::Json, &Context::new()).is_err(), "not a listed var");
    for bad in
        ["{{motd", "{{#a}}x", "{{/a}}", "{{#a}}{{/b}}", "{{env.HOME}}", "{{> partial}}", "{{a b}}"]
    {
        assert!(Template::parse(bad).is_err(), "{bad}");
    }
    let deep = "{{#a}}".repeat(17) + &"{{/a}}".repeat(17);
    assert!(Template::parse(&deep).is_err(), "nesting is bounded");
}

#[test]
fn a_value_never_leaves_its_place_in_any_format() {
    let hostile = text("x\"\n[admin]\nop=true <a/> ' & $(id) `id` \\");
    let c = ctx(&[("v", hostile)]);
    let render = |f, src: &str| Template::parse(src).unwrap().render(f, &c);
    let json = render(TemplateFormat::Json, "{\"motd\": {{v}}}").unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed["motd"], "x\"\n[admin]\nop=true <a/> ' & $(id) `id` \\");
    let toml = render(TemplateFormat::Toml, "motd = {{v}}").unwrap();
    assert!(!toml.contains('\n'), "{toml}");
    let yaml = render(TemplateFormat::Yaml, "motd: {{v}}").unwrap();
    assert!(!yaml.contains('\n') && yaml.starts_with("motd: \""), "{yaml}");
    let props = render(TemplateFormat::Properties, "motd={{v}}").unwrap();
    assert!(!props.contains('\n') && props.contains("\\n[admin]"), "{props}");
    let xml = render(TemplateFormat::Xml, "<motd>{{v}}</motd>").unwrap();
    assert!(xml.contains("&lt;a/&gt;") && xml.contains("&quot;") && xml.contains("&amp;"));
    assert!(render(TemplateFormat::Ini, "motd={{v}}").is_err(), "INI cannot hold a newline");
    assert!(render(TemplateFormat::Env, "MOTD={{v}}").is_err(), "nor can an env file");
    let one_line = ctx(&[("v", text("a\"b $HOME `x` \\"))]);
    let env = Template::parse("MOTD={{v}}").unwrap().render(TemplateFormat::Env, &one_line);
    assert_eq!(env.unwrap(), "MOTD=\"a\\\"b \\$HOME \\`x\\` \\\\\"");
}

#[test]
fn properties_read_the_same_in_any_encoding_and_keep_leading_space() {
    let c = ctx(&[("v", text(" Grüße ✓"))]);
    let out = Template::parse("motd={{v}}").unwrap().render(TemplateFormat::Properties, &c);
    assert_eq!(out.unwrap(), "motd=\\ Gr\\u00FC\\u00DFe \\u2713");
    let emoji = ctx(&[("v", text("🙂"))]);
    let out = Template::parse("{{v}}").unwrap().render(TemplateFormat::Properties, &emoji);
    assert_eq!(out.unwrap(), "\\uD83D\\uDE42", "a surrogate pair, as Java reads it");
}

#[test]
fn numbers_and_booleans_stand_bare_and_unset_is_the_format_s_nothing() {
    let c =
        ctx(&[("n", Var::Number("4294967296".into())), ("b", Var::Bool(true)), ("u", Var::Unset)]);
    let src = "{{n}} {{b}} {{u}}";
    let t = Template::parse(src).unwrap();
    assert_eq!(t.render(TemplateFormat::Json, &c).unwrap(), "4294967296 true null");
    assert_eq!(t.render(TemplateFormat::Toml, &c).unwrap(), "4294967296 true \"\"");
    assert_eq!(t.render(TemplateFormat::Properties, &c).unwrap(), "4294967296 true ");
}
