// SPDX-License-Identifier: AGPL-3.0-or-later

//! Bytes from a pipe, cut into records at line ends. A record is the bytes as written, its line
//! end included (`\n`, or `\r\n` from a terminal), so a reader reproduces the output exactly and
//! can tell a finished line from a prompt still waiting for input, which the caller flushes as a
//! record of its own with no line end (§2 of docs/14-streams.md). A line longer than the stream's
//! largest record is kept up to that size and the rest of it dropped and counted, so a workload
//! that prints a 200 MB line costs a gap record rather than the memory to hold it (question 5 of
//! §9).

/// What the splitter yields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Piece {
    /// A line with its line end, or what was flushed of an unfinished one without.
    Line(Vec<u8>),
    /// Bytes of an overlong line that were dropped.
    Dropped(u64),
}

/// Cuts one pipe's bytes into lines.
#[derive(Debug)]
pub struct Lines {
    max: usize,
    held: Vec<u8>,
    /// Bytes dropped from the line being read, since it outgrew `max`.
    dropping: Option<u64>,
}

impl Lines {
    /// Records of at most `max` bytes, line end included.
    #[must_use]
    pub fn new(max: usize) -> Self {
        // Room is kept for the longest line end, `\r\n`, so a full-length line keeps its end.
        Self { max: max.saturating_sub(2).max(1), held: Vec::new(), dropping: None }
    }

    /// Takes in `bytes`; the pieces they complete.
    pub fn push(&mut self, bytes: &[u8]) -> Vec<Piece> {
        let mut out = Vec::new();
        let mut rest = bytes;
        while !rest.is_empty() {
            let (chunk, ended, after) = match rest.iter().position(|b| *b == b'\n') {
                Some(i) => {
                    (rest.get(..i).unwrap_or_default(), true, rest.get(i + 1..).unwrap_or_default())
                }
                None => (rest, false, &[][..]),
            };
            self.take(chunk, &mut out);
            if ended {
                self.end_line(&mut out);
            }
            rest = after;
        }
        out
    }

    fn take(&mut self, chunk: &[u8], out: &mut Vec<Piece>) {
        if let Some(dropped) = &mut self.dropping {
            *dropped += u64::try_from(chunk.len()).unwrap_or(u64::MAX);
            return;
        }
        let room = self.max - self.held.len();
        if chunk.len() <= room {
            self.held.extend_from_slice(chunk);
            return;
        }
        self.held.extend_from_slice(chunk.get(..room).unwrap_or_default());
        out.push(Piece::Line(std::mem::take(&mut self.held)));
        self.dropping = Some(u64::try_from(chunk.len() - room).unwrap_or(u64::MAX));
    }

    fn end_line(&mut self, out: &mut Vec<Piece>) {
        if let Some(dropped) = self.dropping.take() {
            out.push(Piece::Dropped(dropped));
            return;
        }
        let mut line = std::mem::take(&mut self.held);
        line.push(b'\n');
        out.push(Piece::Line(line));
    }

    /// What is held of an unfinished line, as a piece of its own.
    pub fn flush(&mut self) -> Option<Piece> {
        if let Some(dropped) = self.dropping.take() {
            return Some(Piece::Dropped(dropped));
        }
        (!self.held.is_empty()).then(|| Piece::Line(std::mem::take(&mut self.held)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_are_cut_at_their_ends_kept_whole_and_an_overlong_one_is_counted() {
        let mut l = Lines::new(10);
        assert_eq!(l.push(b"hel"), []);
        assert_eq!(
            l.push(b"lo\r\nworld\nthis line is long\nok"),
            [
                Piece::Line(b"hello\r\n".to_vec()),
                Piece::Line(b"world\n".to_vec()),
                Piece::Line(b"this lin".to_vec()),
                Piece::Dropped(9),
            ]
        );
        assert_eq!(l.flush(), Some(Piece::Line(b"ok".to_vec())), "a prompt is a record, unended");
        assert_eq!(l.flush(), None);
        assert_eq!(l.push(b"\n"), [Piece::Line(b"\n".to_vec())], "an empty line is a line");
    }
}
