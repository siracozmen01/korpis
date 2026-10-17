// SPDX-License-Identifier: AGPL-3.0-or-later

//! The panel's files (§1 of docs/11-surface-web.md): the static bundle in `--web-root`, served on
//! the API's address to every path that is not a procedure of the API. A path names a file
//! beneath the root or nothing: one that climbs out, or reaches a hidden file, is not found. A
//! path with no file and no extension is a route of the panel's own, answered with its
//! `index.html`; the hashed assets are kept by a browser for a year, everything else asked again.

use bytes::Bytes;
use http::{HeaderValue, Method, Response, StatusCode, header};
use std::path::{Component, Path, PathBuf};

/// A file's answer: its bytes, what they are, and how long a browser may keep them.
pub struct Served {
    /// The status.
    pub status: StatusCode,
    /// The bytes; empty for `HEAD`.
    pub body: Bytes,
    /// The content type.
    pub content_type: &'static str,
    /// How long it may be kept.
    pub cache: &'static str,
}

/// How long a hashed asset may be kept: a year, since a change gives it a new name.
const IMMUTABLE: &str = "public, max-age=31536000, immutable";
/// Everything else: kept, but asked about again before it is used.
const REVALIDATE: &str = "no-cache";

/// The file under `root` that `path` names, read for `method`.
pub async fn serve(root: &Path, method: &Method, path: &str) -> Served {
    if method != Method::GET && method != Method::HEAD {
        return text(StatusCode::METHOD_NOT_ALLOWED, "only GET and HEAD are served here");
    }
    let Some(relative) = beneath(path) else {
        return text(StatusCode::NOT_FOUND, "not found");
    };
    let mut file = root.join(&relative);
    let mut found = tokio::fs::read(&file).await;
    if found.is_err() && relative.extension().is_none() {
        file = root.join("index.html");
        found = tokio::fs::read(&file).await;
    }
    let Ok(bytes) = found else { return text(StatusCode::NOT_FOUND, "not found") };
    if !within(root, &file).await {
        return text(StatusCode::NOT_FOUND, "not found");
    }
    let cache = if relative.starts_with("assets") { IMMUTABLE } else { REVALIDATE };
    let body = if method == Method::HEAD { Bytes::new() } else { Bytes::from(bytes) };
    Served { status: StatusCode::OK, body, content_type: content_type(&file), cache }
}

/// `path` as a relative path beneath the root, `index.html` for the root itself; none if any
/// part of it climbs, is absolute, or names a hidden file.
fn beneath(path: &str) -> Option<PathBuf> {
    let trimmed = path.trim_start_matches('/');
    if trimmed.is_empty() {
        return Some(PathBuf::from("index.html"));
    }
    let mut relative = PathBuf::new();
    for part in Path::new(trimmed).components() {
        match part {
            Component::Normal(p) if !p.to_string_lossy().starts_with('.') => relative.push(p),
            _ => return None,
        }
    }
    Some(relative)
}

/// Whether `file`, its links followed, is still beneath `root`: a link in the bundle that points
/// out of it serves nothing.
async fn within(root: &Path, file: &Path) -> bool {
    match (tokio::fs::canonicalize(root).await, tokio::fs::canonicalize(file).await) {
        (Ok(root), Ok(file)) => file.starts_with(root),
        _ => false,
    }
}

fn content_type(file: &Path) -> &'static str {
    match file.extension().and_then(|e| e.to_str()) {
        Some("html") => "text/html; charset=utf-8",
        Some("js" | "mjs") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json" | "map") => "application/json",
        Some("webmanifest") => "application/manifest+json",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("ico") => "image/x-icon",
        Some("woff2") => "font/woff2",
        Some("txt") => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

/// The answer to a path that is not the API's when no panel is served.
#[must_use]
pub fn alone() -> Served {
    text(StatusCode::NOT_FOUND, "no panel is served at this address: the API alone is")
}

fn text(status: StatusCode, message: &'static str) -> Served {
    Served {
        status,
        body: Bytes::from_static(message.as_bytes()),
        content_type: "text/plain; charset=utf-8",
        cache: REVALIDATE,
    }
}

/// `served` as a response, with the headers every file of the panel carries: no sniffing of
/// its type, no referrer sent onward, and never framed by another page.
pub fn response(served: Served) -> Response<Bytes> {
    let mut r = Response::new(served.body);
    *r.status_mut() = served.status;
    let h = r.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static(served.content_type));
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static(served.cache));
    h.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    h.insert(header::REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    h.insert(header::CONTENT_SECURITY_POLICY, HeaderValue::from_static("frame-ancestors 'none'"));
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_names_a_file_beneath_the_root_or_nothing() {
        assert_eq!(beneath("/"), Some(PathBuf::from("index.html")));
        assert_eq!(beneath("/assets/index-1.js"), Some(PathBuf::from("assets/index-1.js")));
        assert_eq!(beneath("/../etc/passwd"), None);
        assert_eq!(beneath("/assets/../../x"), None);
        assert_eq!(beneath("/.git/config"), None);
        assert_eq!(beneath("//etc/passwd").map(|p| p.is_relative()), Some(true));
    }

    #[tokio::test]
    async fn a_route_of_the_panel_is_its_index_and_a_missing_asset_is_not_found() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("index.html"), "<p>panel</p>").unwrap();
        std::fs::create_dir(root.path().join("assets")).unwrap();
        std::fs::write(root.path().join("assets/a-1.js"), "x").unwrap();
        let route = serve(root.path(), &Method::GET, "/organizations/acme").await;
        assert_eq!(
            (route.status, route.content_type),
            (StatusCode::OK, "text/html; charset=utf-8")
        );
        assert_eq!(route.cache, REVALIDATE);
        let asset = serve(root.path(), &Method::GET, "/assets/a-1.js").await;
        assert_eq!(asset.cache, IMMUTABLE);
        let missing = serve(root.path(), &Method::GET, "/assets/gone.js").await;
        assert_eq!(missing.status, StatusCode::NOT_FOUND);
        let post = serve(root.path(), &Method::POST, "/").await;
        assert_eq!(post.status, StatusCode::METHOD_NOT_ALLOWED);
    }
}
