// SPDX-License-Identifier: AGPL-3.0-or-later

//! The panel on the API's address (§1 and §7 of docs/11-surface-web.md): its files from
//! `--web-root` beside the API, a route of its own answered with its index, the hashed assets kept
//! and the rest asked again; `/about/source` naming both builds; cross-origin calls answered for
//! the listed origins alone; and without `--web-root`, no panel at all.

use crate::panel::PANEL;
use crate::support::Harness;
use reqwest::{Client, Method, StatusCode};

/// A second replica of `h`'s control plane, serving the panel in `root`; its address.
async fn serving(h: &Harness, root: Option<&std::path::Path>) -> String {
    let config = korpis_server::ServeConfig {
        listen: "127.0.0.1:0".parse().unwrap(),
        web_root: root.map(std::path::Path::to_path_buf),
        ..h.served.clone()
    };
    let running = korpis_server::serve(config).await.expect("a replica serving the panel");
    format!("https://localhost:{}", running.addr.port())
}

fn client(h: &Harness) -> Client {
    let ca = reqwest::Certificate::from_pem(&h.ca_pem).unwrap();
    Client::builder().add_root_certificate(ca).build().unwrap()
}

fn bundle() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("index.html"), "<!doctype html><p>panel</p>").unwrap();
    std::fs::create_dir_all(root.path().join("assets")).unwrap();
    std::fs::write(root.path().join("assets/app-1a2b.js"), "export {}").unwrap();
    std::fs::create_dir_all(root.path().join("about")).unwrap();
    let source = r#"{"revision": "panel-rev-1", "source": "https://example.com/korpis"}"#;
    std::fs::write(root.path().join("about/source.json"), source).unwrap();
    root
}

#[tokio::test]
async fn the_panel_is_served_beside_the_api_with_its_source_named() {
    let h = Harness::new().await;
    let root = bundle();
    let base = serving(&h, Some(root.path())).await;
    let c = client(&h);

    let index = c.get(format!("{base}/")).send().await.unwrap();
    assert_eq!(index.status(), StatusCode::OK);
    assert_eq!(index.headers()["cache-control"], "no-cache");
    assert_eq!(index.headers()["x-content-type-options"], "nosniff");
    let route = c.get(format!("{base}/organizations/acme")).send().await.unwrap();
    assert!(route.text().await.unwrap().contains("panel"), "a route of the panel is its index");
    let asset = c.get(format!("{base}/assets/app-1a2b.js")).send().await.unwrap();
    assert!(asset.headers()["cache-control"].to_str().unwrap().contains("immutable"));
    let missing = c.get(format!("{base}/assets/gone.js")).send().await.unwrap();
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);

    let about = c.get(format!("{base}/about/source")).send().await.unwrap().text().await.unwrap();
    assert!(about.contains(korpis_core::build::SOURCE_URL), "{about}");
    assert!(about.contains("panel-rev-1"), "{about}");

    let info = c
        .post(format!("{base}/korpis.v1.ServerInfoService/GetServerInfo"))
        .header("content-type", "application/json")
        .body("{}")
        .send()
        .await
        .unwrap();
    assert_eq!(info.status(), StatusCode::OK, "the API still answers on the same address");
}

#[tokio::test]
async fn cross_origin_calls_are_answered_for_the_listed_origins_alone() {
    let h = Harness::new().await;
    let base = serving(&h, None).await;
    let c = client(&h);
    let procedure = format!("{base}/korpis.v1.ServerInfoService/GetServerInfo");
    let preflight = |origin: &str| {
        c.request(Method::OPTIONS, &procedure)
            .header("origin", origin)
            .header("access-control-request-method", "POST")
            .header("access-control-request-headers", "content-type")
            .send()
    };
    let listed = preflight(PANEL).await.unwrap();
    assert_eq!(listed.status(), StatusCode::NO_CONTENT);
    assert_eq!(listed.headers()["access-control-allow-origin"], PANEL);
    assert_eq!(listed.headers()["access-control-allow-credentials"], "true");
    let stranger = preflight("https://elsewhere.example").await.unwrap();
    assert_eq!(stranger.status(), StatusCode::FORBIDDEN);
    assert!(stranger.headers().get("access-control-allow-origin").is_none());

    let called = c
        .post(&procedure)
        .header("origin", PANEL)
        .header("content-type", "application/json")
        .body("{}")
        .send()
        .await
        .unwrap();
    assert_eq!(called.headers()["access-control-allow-origin"], PANEL);

    let none = c.get(format!("{base}/")).send().await.unwrap();
    assert_eq!(none.status(), StatusCode::NOT_FOUND, "no --web-root, no panel");
    let about = c.get(format!("{base}/about/source")).send().await.unwrap();
    assert_eq!(about.status(), StatusCode::OK, "the source is named either way");
}
