// SPDX-License-Identifier: AGPL-3.0-or-later

//! Keyset pagination: a page resumes after the last identifier of the previous one, so pages
//! stay consistent while the collection changes underneath them.

use crate::error::AppError;
use std::future::Future;
use uuid::Uuid;

/// A page request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Page {
    /// Resume after this identifier.
    pub after: Option<Uuid>,
    /// At most this many; bounded by [`Page::MAX`].
    pub size: u32,
}

impl Page {
    /// The size when the client asks for none.
    pub const DEFAULT: u32 = 100;
    /// The largest page served.
    pub const MAX: u32 = 1000;

    /// A page of at most `size` (0 for the default) after `after`.
    #[must_use]
    pub fn new(after: Option<Uuid>, size: u32) -> Self {
        let size = if size == 0 { Self::DEFAULT } else { size.min(Self::MAX) };
        Self { after, size }
    }

    /// The cursor for the page after `items`, or `None` if `items` was the last page.
    #[must_use]
    pub fn next<T>(&self, items: &[T], id: impl Fn(&T) -> Uuid) -> Option<Uuid> {
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
pub struct Paged<T> {
    /// The items.
    pub items: Vec<T>,
    /// Where the next page starts; `None` on the last page.
    pub next: Option<Uuid>,
}

/// A page of what the caller may see: pages of the source after the cursor are read, and what
/// `keep` keeps of each, until the page holds its size or the source runs out. Its cursor is the
/// last item it returns, never one the caller may not see, so a cursor says nothing of what was
/// left out, nor of how much (§8 of docs/10-api.md); what lies past it is read again next time.
pub(crate) async fn visible<T, R, RF, K, KF>(
    page: Page,
    id: impl Fn(&T) -> Uuid,
    mut read: R,
    mut keep: K,
) -> Result<Paged<T>, AppError>
where
    R: FnMut(Page) -> RF,
    RF: Future<Output = Result<Vec<T>, AppError>>,
    K: FnMut(Vec<T>) -> KF,
    KF: Future<Output = Result<Vec<T>, AppError>>,
{
    let size = usize::try_from(page.size).unwrap_or(usize::MAX);
    let mut items = Vec::new();
    let mut after = page.after;
    loop {
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_are_bounded_and_only_a_full_page_has_a_next() {
        assert_eq!(Page::new(None, 0).size, Page::DEFAULT);
        assert_eq!(Page::new(None, 5000).size, Page::MAX);
        let page = Page::new(None, 2);
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
        let mut got = visible(Page::new(None, 2), |i| *i, read, keep).await.unwrap();
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
}
