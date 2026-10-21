// SPDX-License-Identifier: AGPL-3.0-or-later

//! Keyset pagination: a page resumes after the last identifier of the previous one, so pages
//! stay consistent while the collection changes underneath them. A list ordered by something
//! other than its identifier, as the effect log is by its ingest clock, pages by a cursor of its
//! own.

use crate::error::AppError;
use std::future::Future;
use uuid::Uuid;

/// A page request, resuming after a cursor `C`: an identifier, unless the list says otherwise.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Page<C = Uuid> {
    /// Resume after this.
    pub after: Option<C>,
    /// At most this many; bounded by [`Page::MAX`].
    pub size: u32,
}

impl Page {
    /// The size when the client asks for none.
    pub const DEFAULT: u32 = 100;
    /// The largest page served.
    pub const MAX: u32 = 1000;
}

impl<C: Copy> Page<C> {
    /// A page of at most `size` (0 for the default) after `after`.
    #[must_use]
    pub fn new(after: Option<C>, size: u32) -> Self {
        let size = if size == 0 { Page::DEFAULT } else { size.min(Page::MAX) };
        Self { after, size }
    }

    /// The cursor for the page after `items`, or `None` if `items` was the last page.
    #[must_use]
    pub fn next<T>(&self, items: &[T], id: impl Fn(&T) -> C) -> Option<C> {
        let full = u32::try_from(items.len()).unwrap_or(u32::MAX) >= self.size;
        if full { items.last().map(id) } else { None }
    }
}

impl Default for Page {
    fn default() -> Self {
        Self::new(None, 0)
    }
}

/// One page of results.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paged<T, C = Uuid> {
    /// The items.
    pub items: Vec<T>,
    /// Where the next page starts; `None` on the last page.
    pub next: Option<C>,
}

/// How many pages of its source one filtered page reads at most, so a caller who may see little
/// of a long list costs a bounded read per call.
pub const VISIBLE_READS: usize = 8;

/// A page of what the caller may see: pages of the source after the cursor are read, and what
/// `keep` keeps of each, until the page holds its size, the source runs out, or
/// [`VISIBLE_READS`] pages were read. Its cursor is the last item it returns when it is full,
/// and otherwise the last row it read, which may be one the caller may not see; the API seals
/// every cursor, so a token says nothing of what was left out (§8 of docs/10-api.md).
pub(crate) async fn visible<T, C, R, RF, K, KF>(
    page: Page<C>,
    id: impl Fn(&T) -> C,
    mut read: R,
    mut keep: K,
) -> Result<Paged<T, C>, AppError>
where
    C: Copy,
    R: FnMut(Page<C>) -> RF,
    RF: Future<Output = Result<Vec<T>, AppError>>,
    K: FnMut(Vec<T>) -> KF,
    KF: Future<Output = Result<Vec<T>, AppError>>,
{
    let size = usize::try_from(page.size).unwrap_or(usize::MAX);
    let mut items = Vec::new();
    let mut after = page.after;
    for _ in 0..VISIBLE_READS {
        let batch = read(Page { after, size: page.size }).await?;
        let more = batch.len() >= size;
        after = batch.last().map(&id);
        items.extend(keep(batch).await?);
        if items.len() >= size {
            items.truncate(size);
            let next = items.last().map(&id);
            return Ok(Paged { items, next });
        }
        if !more || after.is_none() {
            return Ok(Paged { items, next: None });
        }
    }
    // Read as much as one call may: what was found, and the row it stopped after, which the
    // token seals so the caller learns nothing of it (§8 of docs/10-api.md).
    Ok(Paged { items, next: after })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_are_bounded_and_only_a_full_page_has_a_next() {
        assert_eq!(Page::<Uuid>::new(None, 0).size, Page::DEFAULT);
        assert_eq!(Page::<Uuid>::new(None, 5000).size, Page::MAX);
        let page = Page::<Uuid>::new(None, 2);
        let ids = [Uuid::now_v7(), Uuid::now_v7()];
        assert_eq!(page.next(&ids, |u| *u), Some(ids[1]));
        assert_eq!(page.next(&ids[..1], |u| *u), None);
    }

    #[tokio::test]
    async fn a_filtered_page_fills_from_what_is_visible_and_its_cursor_is_never_hidden() {
        // Ten items in order; the caller sees the first and every third one after it.
        let ids: Vec<Uuid> = (0..10).map(|_| Uuid::now_v7()).collect();
        let shown = |id: &Uuid| ids.iter().position(|i| i == id).is_some_and(|n| n % 3 == 0);
        let read = |p: Page| {
            let from = p.after.and_then(|a| ids.iter().position(|i| *i == a)).map_or(0, |n| n + 1);
            let size = usize::try_from(p.size).unwrap_or(usize::MAX);
            std::future::ready(Ok(ids.iter().skip(from).take(size).copied().collect()))
        };
        let keep =
            |batch: Vec<Uuid>| std::future::ready(Ok(batch.into_iter().filter(shown).collect()));
        let (mut seen, mut tokens) = (Vec::new(), Vec::new());
        let mut got = visible(Page::<Uuid>::new(None, 2), |i| *i, read, keep).await.unwrap();
        seen.append(&mut got.items);
        while let Some(next) = got.next {
            tokens.push(next);
            got = visible(Page::new(Some(next), 2), |i| *i, read, keep).await.unwrap();
            seen.append(&mut got.items);
        }
        let expected: Vec<Uuid> = ids.iter().copied().filter(shown).collect();
        assert_eq!(seen, expected, "each visible item once, in order, the rest never");
        assert!(tokens.iter().all(shown), "every cursor is an item the caller sees");
    }

    #[tokio::test]
    async fn a_long_run_of_what_cannot_be_seen_is_read_a_bounded_stretch_at_a_time() {
        // A hundred rows the caller may not see, then one it may.
        let ids: Vec<Uuid> = (0..101).map(|_| Uuid::now_v7()).collect();
        let last = ids.last().copied();
        let read = |p: Page| {
            let from = p.after.and_then(|a| ids.iter().position(|i| *i == a)).map_or(0, |n| n + 1);
            let size = usize::try_from(p.size).unwrap_or(usize::MAX);
            std::future::ready(Ok(ids.iter().skip(from).take(size).copied().collect()))
        };
        let keep = |batch: Vec<Uuid>| {
            std::future::ready(Ok(batch.into_iter().filter(|i| Some(*i) == last).collect()))
        };
        let mut calls = 1;
        let mut got = visible(Page::<Uuid>::new(None, 2), |i| *i, read, keep).await.unwrap();
        assert!(got.items.is_empty() && got.next.is_some(), "stopped, with somewhere to go on");
        while got.items.is_empty() {
            let after = got.next.expect("the list goes on until the visible row");
            got = visible(Page::new(Some(after), 2), |i| *i, read, keep).await.unwrap();
            calls += 1;
        }
        assert_eq!(got.items, [ids[100]]);
        assert_eq!(calls, 101_usize.div_ceil(2 * VISIBLE_READS), "a bounded read per call");
    }
}
