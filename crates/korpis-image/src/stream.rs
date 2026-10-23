// SPDX-License-Identifier: AGPL-3.0-or-later

//! The layers as the first process of a build hands them to its worker, on the worker's standard
//! input: for each, one byte naming its packing and eight its length, then its bytes, and a last
//! byte that ends them. The first reads each layer from the content store and checks it against
//! its digest as it sends it; one that does not match ends the build, and what the worker made of
//! it is thrown away with the rest of the build.

use crate::source::Packing;
use sha2::{Digest as _, Sha256};
use std::fmt::Write as _;
use std::io::{self, Read, Write};
use std::path::Path;

/// What ends the layers.
const END: u8 = 0xff;

/// What comes next on the worker's input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Next {
    /// A layer of this packing and length.
    Layer(Packing, u64),
    /// No more layers.
    End,
}

fn code(p: Packing) -> u8 {
    match p {
        Packing::Tar => 0,
        Packing::Gzip => 1,
        Packing::Zstd => 2,
    }
}

fn invalid(why: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, why.into())
}

/// Sends the layer at `path`, which must be `size` bytes whose digest is `digest`.
pub fn send(
    to: &mut impl Write,
    path: &Path,
    (digest, size, packing): (&str, u64, Packing),
) -> io::Result<()> {
    to.write_all(&[code(packing)])?;
    to.write_all(&size.to_le_bytes())?;
    let mut from = std::fs::File::open(path)?.take(size);
    let mut hash = Sha256::new();
    let mut buffer = vec![0; 1 << 20];
    let mut sent = 0u64;
    loop {
        let n = from.read(&mut buffer)?;
        let Some(chunk) = buffer.get(..n).filter(|c| !c.is_empty()) else { break };
        hash.update(chunk);
        to.write_all(chunk)?;
        sent += u64::try_from(n).unwrap_or(u64::MAX);
    }
    let mut hex = String::with_capacity(64);
    for b in hash.finalize() {
        let _ = write!(hex, "{b:02x}");
    }
    if sent != size || format!("sha256:{hex}") != digest {
        return Err(invalid(format!("the content store's {digest} does not match its digest")));
    }
    Ok(())
}

/// Says there are no more layers.
pub fn end(to: &mut impl Write) -> io::Result<()> {
    to.write_all(&[END])?;
    to.flush()
}

/// What comes next on `from`.
pub fn next(from: &mut impl Read) -> io::Result<Next> {
    let mut kind = [0; 1];
    from.read_exact(&mut kind)?;
    let packing = match kind {
        [0] => Packing::Tar,
        [1] => Packing::Gzip,
        [2] => Packing::Zstd,
        [END] => return Ok(Next::End),
        [other] => return Err(invalid(format!("a layer packed as {other}"))),
    };
    let mut len = [0; 8];
    from.read_exact(&mut len)?;
    Ok(Next::Layer(packing, u64::from_le_bytes(len)))
}

/// The tar `bytes`, a layer of `packing`, holds; the caller bounds `bytes` to the layer's length
/// and reads what the tar leaves of it.
pub fn unpacked<'a>(bytes: &'a mut dyn Read, packing: Packing) -> io::Result<Box<dyn Read + 'a>> {
    Ok(match packing {
        Packing::Tar => Box::new(bytes),
        Packing::Gzip => Box::new(flate2::read::MultiGzDecoder::new(bytes)),
        Packing::Zstd => Box::new(zstd::stream::read::Decoder::new(bytes)?),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_layer_is_sent_framed_and_one_that_is_not_its_digest_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let blob = dir.path().join("blob");
        std::fs::write(&blob, b"layer bytes").unwrap();
        let digest = korpis_core::content::digest_of(b"layer bytes");
        let mut wire = Vec::new();
        send(&mut wire, &blob, (&digest, 11, Packing::Tar)).unwrap();
        end(&mut wire).unwrap();
        let mut read = wire.as_slice();
        assert_eq!(next(&mut read).unwrap(), Next::Layer(Packing::Tar, 11));
        let mut body = Vec::new();
        let mut layer = (&mut read).take(11);
        unpacked(&mut layer, Packing::Tar).unwrap().read_to_end(&mut body).unwrap();
        assert_eq!(body, b"layer bytes");
        assert_eq!(next(&mut read).unwrap(), Next::End);
        let other = korpis_core::content::digest_of(b"other");
        assert!(send(&mut Vec::new(), &blob, (&other, 11, Packing::Tar)).is_err());
        assert!(send(&mut Vec::new(), &blob, (&digest, 5, Packing::Tar)).is_err(), "short");
    }
}
