// SPDX-License-Identifier: AGPL-3.0-or-later

//! Bytes from a pipe, cut into records at line ends. A line longer than the stream's largest
//! record is kept up to that size and the rest of it dropped and counted, so a workload that
//! prints a 200 MB line costs a gap record rather than the memory to hold it (question 5 of §9 of
//! docs/14-streams.md). A line still unfinished when the caller flushes, a prompt waiting for
//! input, is a record of its own.

/// What the splitter yields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Piece {
    /// A line, without its line end.
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
    /// Lines of at most `max` bytes.
    #[must_use]
    pub fn new(max: usize) -> Self {
        Self { max: max.max(1), held: Vec::new(), dropping: None }
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
        if line.last() == Some(&b'\r') {
            line.pop();
        }
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
    fn lines_are_cut_at_their_ends_and_an_overlong_one_is_counted() {
        let mut l = Lines::new(8);
        assert_eq!(l.push(b"hel"), []);
        assert_eq!(
            l.push(b"lo\r\nworld\nthis line is long\nok"),
            [
                Piece::Line(b"hello".to_vec()),
                Piece::Line(b"world".to_vec()),
                Piece::Line(b"this lin".to_vec()),
                Piece::Dropped(9),
            ]
        );
        assert_eq!(l.flush(), Some(Piece::Line(b"ok".to_vec())), "a prompt is a record");
        assert_eq!(l.flush(), None);
        assert_eq!(l.push(b"\n"), [Piece::Line(Vec::new())], "an empty line is a line");
    }
}
