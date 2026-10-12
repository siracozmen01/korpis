// SPDX-License-Identifier: AGPL-3.0-or-later

//! Keyset pagination: a page resumes after the last identifier of the previous one, so pages
//! stay consistent while the collection changes underneath them.

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
}
