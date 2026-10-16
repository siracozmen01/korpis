// SPDX-License-Identifier: AGPL-3.0-or-later

//! Password hashing off the threads that serve requests (§9.6 of docs/08-identity.md). Argon2id
//! at OWASP's parameters takes tens of milliseconds and 19 MiB each time, on purpose; run on a
//! request thread it would stall every other request that thread serves, and run without a bound
//! a burst of sign-ins would cost memory instead of time. So each hash runs on the blocking pool,
//! at most as many at once as the machine has cores, and the rest wait their turn.

use korpis_app::ports::{PasswordFuture, Passwords};
use korpis_core::account::password::{self, PasswordError};
use std::sync::Arc;
use tokio::sync::Semaphore;
use zeroize::Zeroizing;

/// Hashing on the blocking pool, bounded.
#[derive(Debug)]
pub struct Bounded {
    permits: Arc<Semaphore>,
}

impl Bounded {
    /// At most `at_once` hashes at a time, and at least one.
    #[must_use]
    pub fn new(at_once: usize) -> Self {
        Self { permits: Arc::new(Semaphore::new(at_once.max(1))) }
    }

    /// As many at a time as the machine has cores.
    #[must_use]
    pub fn per_core() -> Self {
        Self::new(std::thread::available_parallelism().map_or(1, std::num::NonZero::get))
    }

    /// `work` on the blocking pool once a permit is free; `failed` if it could not run.
    async fn run<T: Send + 'static>(
        &self,
        work: impl FnOnce() -> T + Send + 'static,
        failed: T,
    ) -> T {
        let Ok(_permit) = self.permits.acquire().await else { return failed };
        tokio::task::spawn_blocking(work).await.unwrap_or(failed)
    }
}

impl Passwords for Bounded {
    fn hash<'a>(&'a self, given: &'a str) -> PasswordFuture<'a, Result<String, PasswordError>> {
        let given = Zeroizing::new(given.to_owned());
        let failed = Err(PasswordError::Hash("the hashing task did not finish".into()));
        Box::pin(self.run(move || password::hash(&given), failed))
    }

    fn matches<'a>(&'a self, given: &'a str, stored: &'a str) -> PasswordFuture<'a, bool> {
        let given = Zeroizing::new(given.to_owned());
        let stored = stored.to_owned();
        Box::pin(self.run(move || password::matches(&given, &stored), false))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn hashes_run_off_the_request_thread_and_match_only_themselves() {
        let passwords = Bounded::new(1);
        let stored = passwords.hash("correct horse battery").await.unwrap();
        let (yes, no) = tokio::join!(
            passwords.matches("correct horse battery", &stored),
            passwords.matches("correct horse batterz", &stored),
        );
        assert!(yes && !no);
        assert_eq!(passwords.hash("short").await, Err(PasswordError::TooShort));
    }
}
