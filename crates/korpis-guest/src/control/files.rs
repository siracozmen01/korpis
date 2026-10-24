// SPDX-License-Identifier: AGPL-3.0-or-later

//! The workload's files as the host sends them into its guest (§6 of docs/04-runtimes.md): its
//! managed files in `config/` and its dependencies' files in `deps/`, which a container sees in
//! its node's directory at `/run/korpis` and a guest is given a copy of. A file is sent in parts,
//! each a frame, and a sync ends with what each directory keeps, the rest removed. The host
//! encodes these and the guest decodes them; what the guest answers is one byte.

/// The most files one directory of a guest's `/run/korpis` holds: their names fit one frame.
pub const MOST_FILES: usize = 200;
/// The longest a file may be.
pub const MOST_FILE: u64 = 1 << 20;
/// The most bytes of a file one part carries, leaving room in a frame for where it goes.
pub const PART: usize = 60 * 1024;

/// Which of the workload's directories a file is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Place {
    /// `config/`, the recipe's managed files (§5.2 of docs/09-recipes.md).
    Config,
    /// `deps/`, its dependencies' files (§5 of docs/07-networking.md).
    Deps,
}

impl Place {
    /// Both, in the order a sync sends them.
    pub const ALL: [Self; 2] = [Self::Config, Self::Deps];

    /// The directory's name in `/run/korpis`.
    #[must_use]
    pub const fn dir(self) -> &'static str {
        match self {
            Self::Config => "config",
            Self::Deps => "deps",
        }
    }

    const fn byte(self) -> u8 {
        match self {
            Self::Config => 0,
            Self::Deps => 1,
        }
    }

    const fn of(byte: u8) -> Option<Self> {
        match byte {
            0 => Some(Self::Config),
            1 => Some(Self::Deps),
            _ => None,
        }
    }
}

/// Whether `name` is one a file of a guest's `/run/korpis` may have: a plain file name, not
/// hidden, since the guest keeps a part being written under a hidden name.
#[must_use]
pub fn plain(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 255
        && !name.starts_with('.')
        && !name.bytes().any(|b| b == b'/' || b == b'\n' || b == 0)
}

/// A part of a file, as a [`super::Kind::Put`] frame carries it: its place, its name, the offset
/// it starts at, and whether it is the last.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Part<'a> {
    /// Which directory.
    pub place: Place,
    /// The file's name in it.
    pub name: &'a str,
    /// Where these bytes start in the file.
    pub offset: u64,
    /// Whether the file ends with these bytes.
    pub last: bool,
    /// The bytes.
    pub data: &'a [u8],
}

impl<'a> Part<'a> {
    /// The payload: the place, the name's length and the name, the offset, the last flag, then
    /// the bytes.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let name = self.name.as_bytes();
        let mut out = Vec::with_capacity(11 + name.len() + self.data.len());
        out.push(self.place.byte());
        out.push(u8::try_from(name.len()).unwrap_or(u8::MAX));
        out.extend_from_slice(name);
        out.extend_from_slice(&self.offset.to_le_bytes());
        out.push(u8::from(self.last));
        out.extend_from_slice(self.data);
        out
    }

    /// The part `payload` holds, if it is one whose name is plain and whose bytes stay within
    /// [`MOST_FILE`].
    #[must_use]
    pub fn decode(payload: &'a [u8]) -> Option<Self> {
        let (&place, rest) = payload.split_first()?;
        let (&length, rest) = rest.split_first()?;
        let (name, rest) = rest.split_at_checked(usize::from(length))?;
        let (offset, rest) = rest.split_first_chunk::<8>()?;
        let (&last, data) = rest.split_first()?;
        let name = std::str::from_utf8(name).ok().filter(|n| plain(n))?;
        let offset = u64::from_le_bytes(*offset);
        let end = offset.checked_add(u64::try_from(data.len()).ok()?)?;
        let last = match last {
            0 => false,
            1 => true,
            _ => return None,
        };
        (end <= MOST_FILE).then_some(Self { place: Place::of(place)?, name, offset, last, data })
    }
}

/// What one directory keeps after a sync, as a [`super::Kind::Keep`] frame carries it: the
/// place, then each name on a line of its own.
#[must_use]
pub fn keep(place: Place, names: &[&str]) -> Vec<u8> {
    let mut out = vec![place.byte()];
    out.extend_from_slice(names.join("\n").as_bytes());
    out
}

/// The place and names `payload` holds, if every name is plain and there are at most
/// [`MOST_FILES`].
#[must_use]
pub fn kept(payload: &[u8]) -> Option<(Place, Vec<&str>)> {
    let (&place, names) = payload.split_first()?;
    let names = std::str::from_utf8(names).ok()?;
    let names: Vec<&str> = if names.is_empty() { Vec::new() } else { names.split('\n').collect() };
    (names.len() <= MOST_FILES && names.iter().all(|n| plain(n)))
        .then_some((Place::of(place)?, names))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "a failing test should fail loudly")]

    use super::*;

    #[test]
    fn a_part_reads_back_and_one_past_the_bounds_or_misnamed_is_refused() {
        let part = Part { place: Place::Deps, name: "db.json", offset: 7, last: true, data: b"{}" };
        assert_eq!(Part::decode(&part.encode()), Some(part.clone()));
        for name in ["", ".db.json.part", "../x", "a/b", "a\nb"] {
            assert_eq!(Part::decode(&Part { name, ..part.clone() }.encode()), None, "{name:?}");
        }
        let far = Part { offset: MOST_FILE - 1, ..part.clone() };
        assert_eq!(Part::decode(&far.encode()), None, "past the most a file may be");
        let mut flag = part.encode();
        *flag.get_mut(11 + "db.json".len() - 1).unwrap() = 2;
        assert_eq!(Part::decode(&flag), None, "a flag is a flag");
    }

    #[test]
    fn what_a_directory_keeps_reads_back_and_none_is_none() {
        let names = ["app.toml", "motd.txt"];
        assert_eq!(kept(&keep(Place::Config, &names)), Some((Place::Config, names.to_vec())));
        assert_eq!(kept(&keep(Place::Deps, &[])), Some((Place::Deps, vec![])));
        assert_eq!(kept(&[0, b'.', b'x']), None, "a hidden name");
        assert_eq!(kept(&[2]), None, "an unknown place");
    }
}
