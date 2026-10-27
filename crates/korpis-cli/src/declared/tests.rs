// SPDX-License-Identifier: AGPL-3.0-or-later

use super::*;

const SURVIVAL: &str = r#"workload: mc-survival
project: community-servers
recipe:  ghcr.io/korpis/recipes/minecraft-paper@sha256:9d2f000000000000000000000000000000000000000000000000000000000000
lifecycle: persistent
console: interactive
resources:
  memory: { reservation: 4Gi, limit: 6Gi }
  cpu:    { reservation: 2 }
volumes:
  - name: world
    size: 40Gi
    class: local
    replicas: 1
    replication_interval: 5m
endpoints:
  - name: game
    port: 25565
    exposure: stable
continuity:
  replica_failover: automatic
config:
  motd: "Survival · season 4"
  difficulty: hard
  max_players: 40
"#;

#[test]
fn the_example_of_the_document_is_a_body() {
    let d: Declared = serde_saphyr::from_str(SURVIVAL).unwrap();
    let b = d.body().unwrap();
    let r = b.recipe.as_option().unwrap();
    assert_eq!(r.name, "ghcr.io/korpis/recipes/minecraft-paper");
    assert!(r.digest.starts_with("sha256:9d2f") && r.version.is_empty());
    assert_eq!(b.lifecycle.as_known(), Some(pb::Lifecycle::LIFECYCLE_PERSISTENT));
    let res = b.resources.as_option().unwrap();
    assert_eq!(res.memory.as_option().unwrap().limit, 6 << 30);
    assert_eq!(res.cpu.as_option().unwrap().reservation_millis, 2000);
    assert_eq!(b.volumes[0].size_bytes, 40 << 30);
    assert_eq!(b.volumes[0].storage_class, "local");
    let every = b.volumes[0].replication_interval.as_option().map(|d| d.seconds);
    assert_eq!((b.volumes[0].replicas, every), (1, Some(300)));
    let continuity = b.continuity.as_option().unwrap();
    let automatic = Some(pb::ReplicaFailover::REPLICA_FAILOVER_AUTOMATIC);
    assert_eq!(continuity.replica_failover.as_known(), automatic);
    assert_eq!(b.endpoints[0].exposure.as_known(), Some(pb::Exposure::EXPOSURE_STABLE));
    assert_eq!(b.config["max_players"].kind, Some(V::IntValue(40)));
    assert_eq!(b.config["difficulty"].kind, Some(V::StringValue("hard".into())));
}

#[test]
fn a_tag_is_a_version_and_anything_unknown_is_refused() {
    let d: Declared =
        serde_saphyr::from_str("workload: a\nproject: p\nrecipe: reg:5000/x/y:1.2\n").unwrap();
    let r = d.body().unwrap().recipe.into_option().unwrap();
    assert_eq!((r.name.as_str(), r.version.as_str()), ("reg:5000/x/y", "1.2"));
    let typo =
        serde_saphyr::from_str::<Declared>("workload: a\nproject: p\nrecipe: x:1\nvolume: []\n");
    assert!(typo.unwrap_err().to_string().contains("volume"));
    let bad: Declared =
        serde_saphyr::from_str("workload: a\nproject: p\nrecipe: x:1\nlifecycle: forever\n")
            .unwrap();
    assert!(bad.body().is_err(), "an enum the API does not have");
}

#[test]
fn a_key_is_found_on_its_line() {
    assert_eq!(line_of(SURVIVAL, Some("config"), "difficulty"), Some(23));
    assert_eq!(line_of(SURVIVAL, None, "workload"), Some(1));
    assert_eq!(line_of(SURVIVAL, Some("config"), "absent"), None);
}

const APART: &str = r"workload: left
project: games
recipe: reg/waiter:1
labels: {app: pair}
placement:
  node_selector:
    match_labels: {region: fra}
    match_expressions:
      - {key: disk, operator: in, values: [ssd, nvme]}
  tolerations:
    - {key: gpu, value: 'true'}
  affinity:
    - {anti: true, required: true, topology_key: host, workloads: {match_labels: {app: pair}}}
  preferred_region: fra
";

#[test]
fn a_placement_is_the_body_s_with_its_operators_unprefixed() {
    let d: Declared = serde_saphyr::from_str(APART).unwrap();
    let p = d.body().unwrap().placement.into_option().unwrap();
    let nodes = p.node_selector.as_option().unwrap();
    assert_eq!(nodes.match_labels.get("region").map(String::as_str), Some("fra"));
    let e = &nodes.match_expressions[0];
    assert_eq!(e.operator.as_known(), Some(pb::LabelOperator::LABEL_OPERATOR_IN));
    assert_eq!(e.values, ["ssd", "nvme"]);
    assert_eq!((p.tolerations[0].key.as_str(), p.tolerations[0].value.as_str()), ("gpu", "true"));
    let rule = &p.affinity[0];
    assert!(rule.anti && rule.required && rule.topology_key == "host");
    let pair = rule.workloads.as_option().unwrap().match_labels.get("app");
    assert_eq!(pair.map(String::as_str), Some("pair"));
    assert_eq!(p.preferred_region, "fra");

    let none: Declared = serde_saphyr::from_str("workload: a\nproject: p\nrecipe: x:1\n").unwrap();
    assert!(!none.body().unwrap().placement.is_set(), "no placement unless one is declared");
    let typo = "workload: a\nproject: p\nrecipe: x:1\nplacement: {affinty: []}\n";
    assert!(serde_saphyr::from_str::<Declared>(typo).unwrap_err().to_string().contains("affinty"));
    let unknown = APART.replace("operator: in", "operator: within");
    let unknown: Declared = serde_saphyr::from_str(&unknown).unwrap();
    assert!(unknown.body().is_err(), "an operator the API does not have");
}
