// SPDX-License-Identifier: AGPL-3.0-or-later

//! A container's cumulative counters, read from its cgroup v2 files as the kernel keeps them
//! (§2 of docs/15-observability.md, "Measuring it, as designed"): CPU time and time throttled
//! from `cpu.stat`, bytes read and written from `io.stat` summed over devices, and memory in use
//! and its high-water mark. Each is absent where its file or its line is, since a controller not
//! enabled is a quantity not measured, never a zero.

use korpis_proto::korpis::driver::v1 as driver;

/// A number from one line of a cgroup file.
fn number(text: &str) -> Option<u64> {
    text.trim().parse().ok()
}

/// The value of `key` in a flat keyed file such as `cpu.stat`.
fn keyed(text: &str, key: &str) -> Option<u64> {
    text.lines().find_map(|l| l.strip_prefix(key)?.strip_prefix(' ')).and_then(number)
}

/// The sum of `key=` over every device of an `io.stat`; absent with no device listed.
fn per_device(text: &str, key: &str) -> Option<u64> {
    let mut sum = None;
    for line in text.lines() {
        let value = line.split_whitespace().find_map(|f| f.strip_prefix(key)?.strip_prefix('='));
        if let Some(v) = value.and_then(number) {
            sum = Some(sum.unwrap_or(0_u64).saturating_add(v));
        }
    }
    sum
}

/// The counters a cgroup's files hold, each read by `read`, which answers a file's text or
/// nothing where the file is absent.
pub fn counters(read: impl Fn(&str) -> Option<String>) -> driver::ObjectCounters {
    let cpu = read("cpu.stat");
    let io = read("io.stat");
    driver::ObjectCounters {
        cpu_usec: cpu.as_deref().and_then(|t| keyed(t, "usage_usec")),
        cpu_throttled_usec: cpu.as_deref().and_then(|t| keyed(t, "throttled_usec")),
        io_read_bytes: io.as_deref().and_then(|t| per_device(t, "rbytes")),
        io_write_bytes: io.as_deref().and_then(|t| per_device(t, "wbytes")),
        memory_bytes: read("memory.current").as_deref().and_then(number),
        memory_peak_bytes: read("memory.peak").as_deref().and_then(number),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn files(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: BTreeMap<String, String> =
            pairs.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect();
        move |f| map.get(f).cloned()
    }

    #[test]
    fn each_counter_is_read_from_the_kernel_s_own_files() {
        let read = files(&[
            ("cpu.stat", "usage_usec 1500\nuser_usec 1000\nnr_throttled 2\nthrottled_usec 40\n"),
            ("io.stat", "8:0 rbytes=100 wbytes=20 rios=1 wios=1\n259:0 rbytes=5 wbytes=7 rios=1\n"),
            ("memory.current", "4096\n"),
            ("memory.peak", "8192\n"),
        ]);
        let c = counters(read);
        assert_eq!((c.cpu_usec, c.cpu_throttled_usec), (Some(1500), Some(40)));
        assert_eq!((c.io_read_bytes, c.io_write_bytes), (Some(105), Some(27)));
        assert_eq!((c.memory_bytes, c.memory_peak_bytes), (Some(4096), Some(8192)));
    }

    #[test]
    fn what_the_kernel_does_not_keep_is_absent_not_zero() {
        let c = counters(files(&[("cpu.stat", "usage_usec 9\n"), ("io.stat", "")]));
        assert_eq!(c.cpu_usec, Some(9));
        assert_eq!(c.cpu_throttled_usec, None, "no cpu controller limit, no throttled line");
        assert_eq!((c.io_read_bytes, c.memory_peak_bytes), (None, None));
        assert_eq!(keyed("usage_usecs 4\n", "usage_usec"), None, "a key is a whole word");
    }
}
