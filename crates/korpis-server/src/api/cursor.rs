// SPDX-License-Identifier: AGPL-3.0-or-later

//! Page tokens (§8 of docs/10-api.md). A token resumes a list after a row, and that row may be one
//! the caller may not see, when a filtered page stops reading before it is full; so a token is
//! the row's identifier sealed under a key every control plane of the installation shares, with
//! the kind it pages bound in, and says nothing to the caller but "continue here". A token that
//! does not open, or opens for another kind, is refused as invalid.

use super::error;
use super::{Api, ConnectError, Page, pb};
use crate::vault::{open_bytes, seal_bytes};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use korpis_app::ports::EffectCursor;
use korpis_core::id::IdKind;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use uuid::Uuid;
use zeroize::Zeroizing;

/// The name the page-token key is kept under (migration 0093).
pub const CURSOR_KEY: &str = "page-token-1";

/// The key page tokens are sealed under.
pub struct Cursors {
    key: Zeroizing<[u8; 32]>,
}

impl std::fmt::Debug for Cursors {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Cursors").finish_non_exhaustive()
    }
}

/// What an effect-log token is sealed for, beside the kinds `context` names.
const EFFECT_CONTEXT: &str = "korpis page token effect";
/// What a usage token is sealed for.
const USAGE_CONTEXT: &str = "korpis page token usage";

fn context(kind: IdKind) -> String {
    format!("korpis page token {}", kind.prefix())
}

impl Cursors {
    /// Tokens sealed under `key`.
    #[must_use]
    pub fn new(key: [u8; 32]) -> Self {
        Self { key: Zeroizing::new(key) }
    }

    /// A key of this process's own, for an API no installation key was given to: its tokens
    /// open only in this process.
    #[must_use]
    pub fn of_this_process() -> Self {
        let mut key = [0u8; 32];
        key[..16].copy_from_slice(Uuid::new_v4().as_bytes());
        key[16..].copy_from_slice(Uuid::new_v4().as_bytes());
        Self::new(key)
    }

    /// The token resuming a list of `kind` after `after`.
    pub fn seal(&self, kind: IdKind, after: Uuid) -> Result<String, ConnectError> {
        let sealed = seal_bytes(&self.key, &context(kind), after.as_bytes())
            .map_err(|e| error::internal(&e))?;
        Ok(URL_SAFE_NO_PAD.encode(sealed))
    }

    /// The token resuming the effect log after `after`: its ingest time and identifier, sealed as
    /// any other list's row is, since a filtered page of the log may stop on an effect the
    /// caller may not see (§4 of docs/15-observability.md).
    pub fn seal_effect(&self, after: EffectCursor) -> Result<String, ConnectError> {
        self.seal_timed(EFFECT_CONTEXT, (after.at, after.id))
    }

    /// The effect a token of the effect log resumes after; `None` for one this key did not seal.
    #[must_use]
    pub fn open_effect(&self, token: &str) -> Option<EffectCursor> {
        let (at, id) = self.open_timed(EFFECT_CONTEXT, token)?;
        Some(EffectCursor { at, id })
    }

    /// The token resuming a listing of usage after a record: its interval start and identifier.
    pub fn seal_usage(&self, after: (SystemTime, Uuid)) -> Result<String, ConnectError> {
        self.seal_timed(USAGE_CONTEXT, after)
    }

    /// The record a token of a usage listing resumes after; `None` for one this key did not seal.
    #[must_use]
    pub fn open_usage(&self, token: &str) -> Option<(SystemTime, Uuid)> {
        self.open_timed(USAGE_CONTEXT, token)
    }

    fn seal_timed(&self, for_: &str, (at, id): (SystemTime, Uuid)) -> Result<String, ConnectError> {
        let micros = at.duration_since(UNIX_EPOCH).unwrap_or_default().as_micros();
        let micros = u64::try_from(micros).unwrap_or(u64::MAX);
        let mut plain = micros.to_be_bytes().to_vec();
        plain.extend_from_slice(id.as_bytes());
        let sealed = seal_bytes(&self.key, for_, &plain).map_err(|e| error::internal(&e))?;
        Ok(URL_SAFE_NO_PAD.encode(sealed))
    }

    fn open_timed(&self, for_: &str, token: &str) -> Option<(SystemTime, Uuid)> {
        let sealed = URL_SAFE_NO_PAD.decode(token).ok()?;
        let plain = open_bytes(&self.key, for_, &sealed).ok()?;
        let (micros, id) = plain.split_at_checked(8)?;
        let micros = u64::from_be_bytes(micros.try_into().ok()?);
        Some((UNIX_EPOCH + Duration::from_micros(micros), Uuid::from_slice(id).ok()?))
    }

    /// The row a token of `kind` resumes after; `None` for a token this key did not seal for it.
    #[must_use]
    pub fn open(&self, kind: IdKind, token: &str) -> Option<Uuid> {
        let sealed = URL_SAFE_NO_PAD.decode(token).ok()?;
        let plain = open_bytes(&self.key, &context(kind), &sealed).ok()?;
        Uuid::from_slice(&plain).ok()
    }
}

impl Api {
    /// A page request, its token opened.
    pub(super) fn page(
        &self,
        p: Option<&pb::PageRequest>,
        kind: IdKind,
    ) -> Result<Page, ConnectError> {
        let (size, token) = p.map_or((0, ""), |p| (p.page_size, p.page_token.as_str()));
        let after = if token.is_empty() {
            None
        } else {
            let refused = || error::invalid("page.page_token", "not a token this list gave");
            Some(self.cursors.open(kind, token).ok_or_else(refused)?)
        };
        Ok(Page::new(after, size))
    }

    /// The page response for a next cursor, sealed.
    pub(super) fn next_page(
        &self,
        next: Option<Uuid>,
        kind: IdKind,
    ) -> Result<pb::PageResponse, ConnectError> {
        Ok(pb::PageResponse {
            next_page_token: next
                .map(|n| self.cursors.seal(kind, n))
                .transpose()?
                .unwrap_or_default(),
            ..Default::default()
        })
    }
}
