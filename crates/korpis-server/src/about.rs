// SPDX-License-Identifier: AGPL-3.0-or-later

//! `/about/source` (§7 of docs/11-surface-web.md, §2 of docs/19-governance.md): where the source
//! of what this address conveys is published, and at which revision. AGPL-3.0 attaches its
//! source obligation to the control plane's binary and to the panel's JavaScript a browser
//! downloads, so the page names both: this build's revision, and the panel's, which its own
//! build writes beside it in `about/source.json`. It is served by the control plane, unstyled,
//! so no theme can remove or restyle it.

use bytes::Bytes;
use http::{HeaderValue, Response, header};
use korpis_core::build::{SOURCE_REVISION, SOURCE_URL, VERSION};
use std::path::Path;

/// The page's path.
pub const PATH: &str = "/about/source";

/// What the panel's build says of itself: its revision and where its source is.
struct Panel {
    revision: String,
    source: String,
}

impl Panel {
    fn read(bytes: &[u8]) -> Option<Self> {
        let v: serde_json::Value = serde_json::from_slice(bytes).ok()?;
        let field = |name: &str| v.get(name)?.as_str().map(str::to_owned);
        Some(Self { revision: field("revision")?, source: field("source")? })
    }
}

/// The page, naming the panel in `web_root` too when one is served and says what it is.
pub async fn page(web_root: Option<&Path>) -> Response<Bytes> {
    let panel = match web_root {
        Some(root) => {
            tokio::fs::read(root.join("about/source.json")).await.ok().and_then(|b| Panel::read(&b))
        }
        None => None,
    };
    let panel_line = match &panel {
        Some(p) => format!(
            "<li>The panel, built from revision <code>{}</code> of <a href=\"{}\">{}</a>.</li>",
            escape(&p.revision),
            escape(&p.source),
            escape(&p.source)
        ),
        None => String::new(),
    };
    let html = format!(
        "<!doctype html>\n<html lang=\"en\"><head><meta charset=\"utf-8\"><title>Source</title>\
         </head><body><h1>Source</h1><p>This is free software under the GNU Affero General \
         Public License, version 3 or later. You may have its source:</p><ul><li>The control \
         plane, Korpis {VERSION}, built from revision <code>{rev}</code> of \
         <a href=\"{SOURCE_URL}\">{SOURCE_URL}</a>.</li>{panel_line}</ul></body></html>\n",
        rev = escape(SOURCE_REVISION),
    );
    let mut r = Response::new(Bytes::from(html));
    let h = r.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static("text/html; charset=utf-8"));
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    h.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("default-src 'none'; frame-ancestors 'none'"),
    );
    r
}

/// `text` safe inside HTML text and attribute values.
fn escape(text: &str) -> String {
    text.chars()
        .map(|c| match c {
            '&' => "&amp;".to_owned(),
            '<' => "&lt;".to_owned(),
            '>' => "&gt;".to_owned(),
            '"' => "&quot;".to_owned(),
            '\'' => "&#39;".to_owned(),
            c => c.to_string(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn the_page_names_this_build_and_the_panel_it_serves() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("about")).unwrap();
        let panel = r#"{"revision": "abc<1>", "source": "https://example.com/korpis"}"#;
        std::fs::write(root.path().join("about/source.json"), panel).unwrap();
        let page = page(Some(root.path())).await;
        let text = String::from_utf8(page.body().to_vec()).unwrap();
        assert!(text.contains(SOURCE_URL) && text.contains(SOURCE_REVISION));
        assert!(text.contains("abc&lt;1&gt;"), "escaped: {text}");
        let alone = page_text(None).await;
        assert!(!alone.contains("The panel"));
    }

    async fn page_text(root: Option<&Path>) -> String {
        String::from_utf8(page(root).await.body().to_vec()).unwrap()
    }
}
