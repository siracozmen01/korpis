// SPDX-License-Identifier: AGPL-3.0-or-later

//! Refresh tokens, kept in the provider's own volume and nowhere else (§9.7 of
//! docs/08-identity.md), and what refreshing one last found. One person is refreshed at a time,
//! and one refresh answers for that person for a minute however many sets ask, because issuers
//! rotate refresh tokens and take a rotated one presented again as theft, revoking the person's
//! grant (Finding 119 of docs/23-walkthroughs.md).

use ring::digest::{SHA256, digest};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::fmt::Write as _;
use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncWriteExt as _;
use tokio::sync::Mutex;
use tokio::time::Instant;

/// How long one refresh answers for its person.
pub const ANSWERS_FOR: Duration = Duration::from_secs(60);
/// How many people are remembered before those whose answer has run out are forgotten.
const REMEMBERED: usize = 1024;

/// What is known of a person now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Standing {
    /// The issuer still knows them: with their groups, when the refresh answered an ID token.
    Known(Option<Vec<String>>),
    /// The issuer refused their refresh token: it no longer knows them.
    Gone,
    /// Nothing current to tell by: no refresh token kept, or the issuer could not be reached.
    Unknown,
}

/// What one refresh at the issuer came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refreshed {
    /// It answered: the person's groups if it answered an ID token for them, and the refresh
    /// token to keep in place of the one spent, if it rotated it.
    Answered { groups: Option<Vec<String>>, token: Option<String> },
    /// It refused the token.
    Refused,
    /// It could not be asked.
    Unreachable,
}

type Last = Arc<Mutex<Option<(Standing, Instant)>>>;

/// The kept tokens, and the last answer for each person.
#[derive(Debug)]
pub struct Refreshes {
    dir: Option<PathBuf>,
    people: std::sync::Mutex<HashMap<String, Last>>,
}

fn file_name(sub: &str) -> String {
    let mut name = String::with_capacity(69);
    for b in digest(&SHA256, sub.as_bytes()).as_ref() {
        let _ = write!(name, "{b:02x}");
    }
    name.push_str(".json");
    name
}

impl Refreshes {
    /// Tokens kept under `dir`; none when there is nowhere to keep them.
    #[must_use]
    pub fn new(dir: Option<PathBuf>) -> Self {
        Self { dir, people: std::sync::Mutex::new(HashMap::new()) }
    }

    /// Keeps `token` as `sub`'s, readable by this process's user alone, replacing what was kept.
    ///
    /// # Errors
    /// It cannot be written.
    pub async fn keep(&self, sub: &str, token: &str) -> std::io::Result<()> {
        let Some(dir) = &self.dir else { return Ok(()) };
        let (path, temporary) =
            (dir.join(file_name(sub)), dir.join(format!("{}.new", file_name(sub))));
        let body = json!({ "sub": sub, "refresh_token": token }).to_string();
        let mut file = tokio::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&temporary)
            .await?;
        file.write_all(body.as_bytes()).await?;
        file.sync_all().await?;
        tokio::fs::rename(&temporary, &path).await
    }

    async fn kept(&self, sub: &str) -> Option<String> {
        let path = self.dir.as_ref()?.join(file_name(sub));
        let body: Value = serde_json::from_slice(&tokio::fs::read(path).await.ok()?).ok()?;
        if body.get("sub").and_then(Value::as_str) != Some(sub) {
            return None;
        }
        body.get("refresh_token").and_then(Value::as_str).map(str::to_owned)
    }

    async fn forget(&self, sub: &str) {
        if let Some(dir) = &self.dir {
            let _ = tokio::fs::remove_file(dir.join(file_name(sub))).await;
        }
    }

    fn last(&self, sub: &str) -> Last {
        let mut people = self.people.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if people.len() > REMEMBERED {
            // Kept while anyone holds it, so no two questions about one person ever find two.
            let fresh = |l: &Option<(Standing, Instant)>| {
                l.as_ref().is_some_and(|(_, at)| at.elapsed() < ANSWERS_FOR)
            };
            people.retain(|_, last| {
                Arc::strong_count(last) > 1 || last.try_lock().is_ok_and(|l| fresh(&l))
            });
        }
        Arc::clone(people.entry(sub.to_owned()).or_default())
    }

    /// What is known of `sub` now: the last answer if it is under a minute old, or else what
    /// one refresh with their kept token finds, asked by `refresh` while every other question
    /// about them waits for it.
    pub async fn standing<F, A>(&self, sub: &str, refresh: F) -> Standing
    where
        F: FnOnce(String) -> A,
        A: Future<Output = Refreshed>,
    {
        let last = self.last(sub);
        let mut last = last.lock().await;
        if let Some((standing, at)) = last.as_ref()
            && at.elapsed() < ANSWERS_FOR
        {
            return standing.clone();
        }
        let Some(token) = self.kept(sub).await else { return Standing::Unknown };
        let standing = match refresh(token).await {
            Refreshed::Answered { groups, token } => {
                if let Some(t) = token
                    && let Err(e) = self.keep(sub, &t).await
                {
                    tracing::warn!(error = %e, "a rotated refresh token could not be kept");
                }
                Standing::Known(groups)
            }
            Refreshed::Refused => {
                self.forget(sub).await;
                Standing::Gone
            }
            Refreshed::Unreachable => Standing::Unknown,
        };
        *last = Some((standing.clone(), Instant::now()));
        standing
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// A refresh of `t1` that takes a while, answering `ops` and rotating it to `t2`.
    async fn rotate(token: String, asked: Arc<AtomicU32>) -> Refreshed {
        asked.fetch_add(1, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(token, "t1", "the token kept, never one already spent");
        Refreshed::Answered { groups: Some(vec!["ops".into()]), token: Some("t2".into()) }
    }

    #[tokio::test]
    async fn a_person_is_refreshed_once_however_many_ask_and_forgotten_when_refused() {
        let dir = tempfile::tempdir().unwrap();
        let r = Arc::new(Refreshes::new(Some(dir.path().to_owned())));
        assert_eq!(r.standing("dana", |_| async { Refreshed::Refused }).await, Standing::Unknown);
        r.keep("dana", "t1").await.unwrap();
        let asked = Arc::new(AtomicU32::new(0));
        let ask = |r: Arc<Refreshes>, asked: Arc<AtomicU32>| async move {
            r.standing("dana", |t| rotate(t, asked)).await
        };
        let (a, b) = tokio::join!(
            ask(Arc::clone(&r), Arc::clone(&asked)),
            ask(Arc::clone(&r), Arc::clone(&asked))
        );
        assert_eq!(asked.load(Ordering::SeqCst), 1, "Finding 119: one refresh for both");
        assert_eq!((a.clone(), b), (Standing::Known(Some(vec!["ops".into()])), a));
        assert_eq!(r.kept("dana").await.as_deref(), Some("t2"), "the rotated token is kept");

        let other = Refreshes::new(Some(dir.path().to_owned()));
        assert_eq!(other.standing("dana", |_| async { Refreshed::Refused }).await, Standing::Gone);
        assert_eq!(other.kept("dana").await, None, "a refused token is forgotten");
    }
}
