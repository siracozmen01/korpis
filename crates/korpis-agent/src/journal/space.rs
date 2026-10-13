// SPDX-License-Identifier: AGPL-3.0-or-later

//! What the journal may discard, and when. A sealed segment is wholly acknowledged once the
//! cursor reaches the number before the next segment's first; from then its retention window
//! runs, and when the window passes the segment is removed. When the bound is reached, wholly
//! acknowledged segments still within their window are removed first, oldest first, because
//! losing what a restored control plane might re-ingest is a smaller loss than losing what no
//! control plane has seen.

use super::Journal;
use crate::durable::sync_dir;
use std::time::SystemTime;

impl Journal {
    /// The highest number sealed segment `i` can hold.
    fn upper(&self, i: usize) -> Option<u64> {
        let next = match self.sealed.get(i.saturating_add(1)) {
            Some(s) => s.first,
            None => self.active.as_ref()?.first,
        };
        Some(next.saturating_sub(1))
    }

    /// Whether every record of sealed segment `i` is acknowledged.
    fn acknowledged(&self, i: usize) -> bool {
        self.upper(i).is_some_and(|u| u <= self.meta.acked)
    }

    /// The control plane has ingested everything through `seq`. A cursor below the one held
    /// means a control plane restored from an older backup: what it has not ingested is no
    /// longer acknowledged, and is kept until it is. A cursor above everything this journal
    /// numbered means the node's own state is older than the control plane's: no record is
    /// numbered at or below it again, so nothing new is mistaken for a duplicate.
    pub fn acknowledge(&mut self, seq: u64, now: SystemTime) -> std::io::Result<()> {
        let mut changed = false;
        if seq >= self.next_seq() {
            tracing::warn!(
                cursor = seq,
                head = self.head,
                "the control plane has ingested past this journal's head; numbering after it"
            );
            self.meta.floor = seq;
            changed = true;
        }
        if seq < self.meta.acked {
            let before = self.meta.acked_at.len();
            self.meta.acked = seq;
            let kept: Vec<u64> = (0..self.sealed.len())
                .filter(|i| self.acknowledged(*i))
                .filter_map(|i| self.sealed.get(i).map(|s| s.first))
                .collect();
            self.meta.acked_at.retain(|first, _| kept.contains(first));
            changed |= self.meta.acked_at.len() != before;
        }
        self.meta.acked = seq;
        changed |= self.note_acknowledged(now);
        if changed {
            self.meta.save(&self.dir.join("state"))?;
        }
        self.discard_expired(now)
    }

    /// Starts the retention window of every sealed segment newly wholly acknowledged.
    pub(super) fn note_acknowledged(&mut self, now: SystemTime) -> bool {
        let fresh: Vec<u64> = (0..self.sealed.len())
            .filter(|i| self.acknowledged(*i))
            .filter_map(|i| self.sealed.get(i).map(|s| s.first))
            .filter(|first| !self.meta.acked_at.contains_key(first))
            .collect();
        for first in &fresh {
            self.meta.acked_at.insert(*first, now);
        }
        !fresh.is_empty()
    }

    /// Removes the oldest sealed segment if it is wholly acknowledged and `may` allows it;
    /// whether it did.
    fn discard_oldest(&mut self, may: impl Fn(SystemTime) -> bool) -> std::io::Result<bool> {
        let Some(oldest) = self.sealed.first().cloned() else { return Ok(false) };
        let since = self.meta.acked_at.get(&oldest.first).copied();
        if !self.acknowledged(0) || !since.is_some_and(may) {
            return Ok(false);
        }
        match std::fs::remove_file(&oldest.path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        sync_dir(&self.dir)?;
        self.sealed.remove(0);
        self.meta.acked_at.remove(&oldest.first);
        if self.reader.is_some_and(|r| r.segment == oldest.first) {
            self.reader = None;
        }
        Ok(true)
    }

    /// Removes every wholly acknowledged segment whose retention window has passed.
    pub(super) fn discard_expired(&mut self, now: SystemTime) -> std::io::Result<()> {
        let retention = self.bounds.retention;
        let mut removed = false;
        while self
            .discard_oldest(|since| now.duration_since(since).unwrap_or_default() >= retention)?
        {
            removed = true;
        }
        if removed {
            self.meta.save(&self.dir.join("state"))?;
        }
        Ok(())
    }

    /// Removes wholly acknowledged segments, oldest first and whatever their window, until
    /// `need` bytes fit below the bound with `reserve` kept free.
    pub(super) fn reclaim(&mut self, need: u64, reserve: u64) -> std::io::Result<()> {
        let mut removed = false;
        while !self.room(need, reserve) && self.discard_oldest(|_| true)? {
            removed = true;
        }
        if removed {
            tracing::warn!("journal at its bound: acknowledged records discarded within retention");
            self.meta.save(&self.dir.join("state"))?;
        }
        Ok(())
    }
}
