// SPDX-License-Identifier: AGPL-3.0-or-later

//! Quantities (`4Gi`, `500m`, `1.5G`) and durations (`1h30m`, `250ms`) as they appear in recipe
//! configuration (§5 of docs/09-recipes.md).
//!
//! The quantity syntax is the Kubernetes one without exponents: a decimal number and an optional
//! suffix, binary (`Ki` to `Ei`), decimal (`k` to `E`), or `m` for thousandths. A quantity is kept
//! in thousandths so that `500m` of a CPU and `4Gi` of memory are both exact integers.

use std::time::Duration;

/// A non-negative quantity, in thousandths of its unit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Quantity {
    millis: u128,
}

/// Why a quantity or duration does not parse.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0:?} is not a valid {1}")]
pub struct ParseError(pub String, pub &'static str);

const SUFFIXES: [(&str, u128); 14] = [
    ("Ki", 1 << 10),
    ("Mi", 1 << 20),
    ("Gi", 1 << 30),
    ("Ti", 1 << 40),
    ("Pi", 1 << 50),
    ("Ei", 1 << 60),
    ("k", 1_000),
    ("M", 1_000_000),
    ("G", 1_000_000_000),
    ("T", 1_000_000_000_000),
    ("P", 1_000_000_000_000_000),
    ("E", 1_000_000_000_000_000_000),
    ("m", 0),
    ("", 1),
];

/// Parses a non-negative decimal as thousandths: at most three fractional digits survive the
/// scaling, so finer fractions are refused rather than rounded.
fn decimal_scaled(number: &str, scale_millis: u128) -> Option<u128> {
    let (whole, frac) = number.split_once('.').unwrap_or((number, ""));
    let all_digits = |s: &str| s.bytes().all(|b| b.is_ascii_digit());
    if whole.is_empty() && frac.is_empty() || !all_digits(whole) || !all_digits(frac) {
        return None;
    }
    let digits = format!("{whole}{frac}");
    let mantissa: u128 = if digits.is_empty() { 0 } else { digits.parse().ok()? };
    let divisor = 10u128.checked_pow(u32::try_from(frac.len()).ok()?)?;
    let scaled = mantissa.checked_mul(scale_millis)?;
    (scaled % divisor == 0).then(|| scaled / divisor)
}

impl Quantity {
    /// Parses a quantity.
    pub fn parse(s: &str) -> Result<Self, ParseError> {
        let bad = || ParseError(s.to_owned(), "quantity");
        let (number, factor) = SUFFIXES
            .iter()
            .find_map(|(suffix, f)| s.strip_suffix(suffix).map(|n| (n, *f)))
            .ok_or_else(bad)?;
        // `m` means thousandths: scale by one rather than by a thousand.
        let scale = if factor == 0 { 1 } else { factor.checked_mul(1000).ok_or_else(bad)? };
        decimal_scaled(number, scale).map(|millis| Self { millis }).ok_or_else(bad)
    }

    /// A whole number of units.
    #[must_use]
    pub fn from_units(units: u64) -> Self {
        Self { millis: u128::from(units) * 1000 }
    }

    /// The value in thousandths of its unit.
    #[must_use]
    pub fn millis(self) -> u128 {
        self.millis
    }

    /// The value in units as a float, for arithmetic over configuration. Exact below 2^53
    /// thousandths, which is every quantity a machine can hold.
    #[must_use]
    #[expect(
        clippy::as_conversions,
        clippy::cast_precision_loss,
        reason = "the documented, deliberate conversion for derived arithmetic"
    )]
    pub fn as_f64(self) -> f64 {
        self.millis as f64 / 1000.0
    }

    /// The value in whole units, rounded down.
    #[must_use]
    pub fn units(self) -> u128 {
        self.millis / 1000
    }

    /// The value in base units as decimal text, exactly: `4Gi` is `4294967296`, `500m` is
    /// `0.5`.
    #[must_use]
    pub fn text(self) -> String {
        let fraction = self.millis % 1000;
        if fraction == 0 {
            return self.units().to_string();
        }
        let digits = format!("{fraction:03}");
        format!("{}.{}", self.units(), digits.trim_end_matches('0'))
    }
}

/// Parses a duration: one or more `<integer><unit>` parts, units `ms`, `s`, `m`, `h`, `d`,
/// largest first, e.g. `1h30m`.
pub fn parse_duration(s: &str) -> Result<Duration, ParseError> {
    let bad = || ParseError(s.to_owned(), "duration");
    let units: [(&str, u64); 5] =
        [("d", 86_400_000), ("h", 3_600_000), ("m", 60_000), ("s", 1_000), ("ms", 1)];
    let mut rest = s;
    let mut total: u64 = 0;
    let mut last_rank = None;
    while !rest.is_empty() {
        let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
        let (number, tail) = rest.split_at(digits);
        let n: u64 = number.parse().map_err(|_| bad())?;
        // `ms` must be tried before `m`.
        let (rank, unit, millis) = [4, 0, 1, 2, 3]
            .into_iter()
            .filter_map(|i| units.get(i).map(|(u, m)| (i, *u, *m)))
            .find(|(_, u, _)| tail.starts_with(u))
            .ok_or_else(bad)?;
        if last_rank.is_some_and(|r| rank <= r) {
            return Err(bad());
        }
        last_rank = Some(rank);
        total = n.checked_mul(millis).and_then(|v| total.checked_add(v)).ok_or_else(bad)?;
        rest = tail.get(unit.len()..).ok_or_else(bad)?;
    }
    if last_rank.is_none() {
        return Err(bad());
    }
    Ok(Duration::from_millis(total))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quantities_are_exact_in_thousandths() {
        assert_eq!(Quantity::parse("4Gi").unwrap().units(), 4 << 30);
        assert_eq!(Quantity::parse("500m").unwrap().millis(), 500);
        assert_eq!(Quantity::parse("1.5G").unwrap().units(), 1_500_000_000);
        assert_eq!(Quantity::parse("2").unwrap(), Quantity::from_units(2));
        assert_eq!(Quantity::parse("0.5").unwrap().millis(), 500);
        for bad in ["", "Gi", "-1", "1.0001", "4gi", "1e3", "1..5"] {
            assert!(Quantity::parse(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn durations_are_ordered_parts() {
        assert_eq!(parse_duration("1h30m").unwrap(), Duration::from_secs(5400));
        assert_eq!(parse_duration("250ms").unwrap(), Duration::from_millis(250));
        assert_eq!(parse_duration("2d").unwrap(), Duration::from_secs(172_800));
        assert_eq!(parse_duration("1m30s").unwrap(), Duration::from_secs(90));
        for bad in ["", "1", "30m1h", "1h1h", "5x", "m"] {
            assert!(parse_duration(bad).is_err(), "{bad:?}");
        }
    }
}
