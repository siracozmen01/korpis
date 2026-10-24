// SPDX-License-Identifier: AGPL-3.0-or-later

//! zstd, decoded in Rust alone (`ruzstd`), as `zstd -d` decodes it: every frame in turn, a
//! skippable frame skipped, and each frame's checksum, where it carries one, checked once the
//! frame ends. A frame asking a window larger than the decoder allows, 128 MiB as the reference
//! decoder's default, is refused rather than given the memory, and no block decodes to more than
//! the format allows one (`ruzstd` 0.9.1, RUSTSEC-2026-0344).

use ruzstd::decoding::errors::{FrameDecoderError, ReadFrameHeaderError};
use ruzstd::decoding::{BlockDecodingStrategy, FrameDecoder};
use std::io::{self, BufRead, Read};

/// The frames of a zstd stream read from `source`, decoded.
pub struct Frames<R> {
    source: R,
    frame: FrameDecoder,
    open: bool,
}

impl<R: BufRead> Frames<R> {
    /// Decodes what `source` holds.
    pub fn new(source: R) -> Self {
        Self { source, frame: FrameDecoder::new(), open: false }
    }

    /// Opens the next frame, skipping skippable ones; whether there was one.
    fn next_frame(&mut self) -> io::Result<bool> {
        while !self.source.fill_buf()?.is_empty() {
            let skip = match self.frame.reset(&mut self.source) {
                Ok(()) => return Ok(true),
                Err(FrameDecoderError::ReadFrameHeaderError(ReadFrameHeaderError::SkipFrame {
                    length,
                    ..
                })) => u64::from(length),
                Err(e) => return Err(io::Error::other(e)),
            };
            let skipped = io::copy(&mut (&mut self.source).take(skip), &mut io::sink())?;
            if skipped != skip {
                return Err(io::Error::other("a skippable frame ends early"));
            }
        }
        Ok(false)
    }

    /// Ends the open frame, its checksum checked.
    fn end_frame(&mut self) -> io::Result<()> {
        self.open = false;
        match (self.frame.get_checksum_from_data(), self.frame.get_calculated_checksum()) {
            (Some(kept), Some(made)) if kept != made => {
                Err(io::Error::other("a zstd frame's checksum does not match its content"))
            }
            _ => Ok(()),
        }
    }
}

impl<R: BufRead> Read for Frames<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        loop {
            if !self.open && !self.next_frame()? {
                return Ok(0);
            }
            self.open = true;
            while self.frame.can_collect() < buf.len() && !self.frame.is_finished() {
                let wanted = BlockDecodingStrategy::UptoBytes(buf.len() - self.frame.can_collect());
                self.frame.decode_blocks(&mut self.source, wanted).map_err(io::Error::other)?;
            }
            let n = self.frame.read(buf)?;
            if n > 0 {
                return Ok(n);
            }
            if self.frame.is_finished() {
                self.end_frame()?;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "a failing test should fail loudly")]

    use super::*;
    use ruzstd::encoding::{CompressionLevel, compress_to_vec};

    fn decoded(bytes: &[u8]) -> io::Result<Vec<u8>> {
        let mut out = Vec::new();
        Frames::new(bytes).read_to_end(&mut out)?;
        Ok(out)
    }

    #[test]
    fn every_frame_is_decoded_and_a_skippable_one_skipped() {
        let mut stream = compress_to_vec(&b"merhaba "[..], CompressionLevel::Fastest);
        stream.extend_from_slice(&0x184d_2a50_u32.to_le_bytes());
        stream.extend_from_slice(&3_u32.to_le_bytes());
        stream.extend_from_slice(b"xyz");
        stream.extend(compress_to_vec(&b"dunya"[..], CompressionLevel::Fastest));
        assert_eq!(decoded(&stream).unwrap(), b"merhaba dunya");
        assert_eq!(decoded(b"").unwrap(), b"");
    }

    #[test]
    fn a_truncated_or_corrupt_stream_is_an_error() {
        let big: Vec<u8> = (0..1 << 16).map(|i| u8::try_from(i % 7).unwrap()).collect();
        let stream = compress_to_vec(&big[..], CompressionLevel::Fastest);
        assert_eq!(decoded(&stream).unwrap(), big);
        assert!(decoded(&stream[..stream.len() / 2]).is_err(), "cut short");
        let mut skip = 0x184d_2a50_u32.to_le_bytes().to_vec();
        skip.extend_from_slice(&9_u32.to_le_bytes());
        assert!(decoded(&skip).is_err(), "a skippable frame cut short");
    }
}
