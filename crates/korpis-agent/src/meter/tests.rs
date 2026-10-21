// SPDX-License-Identifier: AGPL-3.0-or-later

use super::metered::Metered;
use super::*;
use crate::held::HeldSet;
use crate::host::InMemoryHost;
use crate::journal::{Bounds, JournalHandle};
use crate::runtime::memory::InMemory;
use korpis_core::workload::network::host_link;
use korpis_proto::korpis::agent::v1::__buffa::oneof::journal_record::Body;
use korpis_proto::korpis::v1 as pb;
use std::sync::Arc;

struct Node {
    dir: tempfile::TempDir,
    runtime: Metered<InMemory>,
    inner: InMemory,
    host: InMemoryHost,
    meter: Arc<Meter>,
    sampler: Sampler<Metered<InMemory>, InMemoryHost>,
    journal: JournalHandle,
}

fn assignment(workload: &str, epoch: u64) -> agent::Assignment {
    let volume = pb::VolumeSpec { name: "world".into(), ..Default::default() };
    let body = pb::IntentBody { volumes: vec![volume], ..Default::default() };
    agent::Assignment {
        workload: workload.into(),
        lease_epoch: epoch,
        target: pb::Intent { body: body.into(), ..Default::default() }.into(),
        ..Default::default()
    }
}

fn node(workload: &str) -> Node {
    let dir = tempfile::tempdir().unwrap();
    let meter = Arc::new(Meter::open(&dir.path().join("meter"), None).unwrap());
    let inner = InMemory::new();
    let runtime = Metered::new(inner.clone(), meter.clone());
    let host = InMemoryHost::new();
    let mut held = HeldSet::default();
    held.assignments.insert(workload.into(), assignment(workload, 3));
    let (_, held) = tokio::sync::watch::channel(held);
    let journal = JournalHandle::open(&dir.path().join("journal"), Bounds::default()).unwrap();
    let net = dir.path().join("net");
    let told = tokio::sync::watch::channel(None).1;
    let sources = Sources { runtime: runtime.clone(), host: host.clone(), held, net, told };
    let sampler = Sampler::new(meter.clone(), sources, journal.clone(), DEFAULT_INTERVAL);
    Node { dir, runtime, inner, host, meter, sampler, journal }
}

fn cpu(usec: u64, memory: u64) -> driver::ObjectCounters {
    driver::ObjectCounters {
        cpu_usec: Some(usec),
        memory_bytes: Some(memory),
        ..Default::default()
    }
}

async fn object(n: &Node, workload: &str) -> String {
    let spec = driver::PreparedSpec { workload: workload.into(), ..Default::default() };
    n.runtime.create(&spec).await.unwrap()
}

async fn samples(journal: &JournalHandle) -> Vec<agent::MeteringSample> {
    let records = journal.read_after(0, 1 << 20).await.unwrap();
    let metering = records.into_iter().filter_map(|r| match r.body {
        Some(Body::Metering(m)) => Some(*m),
        _ => None,
    });
    metering.collect()
}

fn t(seconds: u64) -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(1_760_000_000 + seconds)
}

#[tokio::test]
async fn a_sample_carries_a_workload_s_use_across_its_objects() {
    let n = node("wl_1");
    let first = object(&n, "wl_1").await;
    n.inner.set_counters(&first, cpu(1_000, 1 << 20));
    n.sampler.read(t(0)).await;
    // Its last counters are read as it is destroyed, before they are gone.
    n.inner.set_counters(&first, cpu(4_000, 1 << 20));
    n.runtime.destroy(&first).await.unwrap();
    let second = object(&n, "wl_1").await;
    n.inner.set_counters(&second, cpu(500, 2 << 20));
    n.sampler.read(t(10)).await;
    n.host.set_usage("wl_1.world", 7_000);
    n.sampler.sample(t(0), DEFAULT_INTERVAL).await;

    let s = samples(&n.journal).await;
    assert_eq!(s.len(), 1);
    let counters = s[0].counters.as_option().unwrap();
    assert_eq!(counters.cpu_usec, Some(4_500), "the first object's whole life and the second's");
    assert_eq!(counters.memory_kib_seconds, Some(1024 * 10 + 2048 * 10));
    assert_eq!((s[0].lease_epoch, s[0].disk_bytes), (3, Some(7_000)));
    assert_eq!(s[0].memory_peak_bytes, Some(2 << 20));
    assert_eq!(counters.net_rx_bytes, None, "no interface, no network meter");
    assert!(s[0].losses.is_empty());
}

#[tokio::test]
async fn an_agent_that_restarts_takes_up_its_accounts_where_it_stopped() {
    let n = node("wl_1");
    let id = object(&n, "wl_1").await;
    n.inner.set_counters(&id, cpu(2_000, 0));
    n.sampler.read(t(0)).await;
    let path = n.dir.path().join("meter");
    let again = Meter::open(&path, Some(t(0) - Duration::from_secs(3600))).unwrap();
    again.track(&id, "wl_1");
    assert_eq!(again.with_book(|b| b.pending("wl_1")).counters.cpu_usec, Some(2_000));

    // A machine that booted after the last reading lost what ran since: a loss, and the next
    // object's count is added to what was read.
    let rebooted = Meter::open(&path, Some(t(60))).unwrap();
    let pending = rebooted.with_book(|b| b.pending("wl_1"));
    assert_eq!(pending.counters.cpu_usec, Some(2_000));
    assert_eq!(pending.losses[0].cause, book::NODE_REBOOTED);
}

#[tokio::test]
async fn a_workload_s_interface_is_metered_from_the_kernel_s_counters() {
    let n = node("wl_1");
    let stats = n.dir.path().join("net").join(host_link("wl_1")).join("statistics");
    std::fs::create_dir_all(&stats).unwrap();
    std::fs::write(stats.join("rx_bytes"), "300\n").unwrap();
    std::fs::write(stats.join("tx_bytes"), "900\n").unwrap();
    n.sampler.read(t(0)).await;
    n.sampler.sample(t(0), DEFAULT_INTERVAL).await;
    let s = samples(&n.journal).await;
    let c = s[0].counters.as_option().unwrap();
    assert_eq!((c.net_tx_bytes, c.net_rx_bytes), (Some(300), Some(900)), "sent, and received");
    assert_eq!(c.cpu_usec, None, "no object read, no CPU meter");
}

#[tokio::test]
async fn memory_unread_for_long_is_not_integrated_and_is_said_lost() {
    let n = node("wl_1");
    let id = object(&n, "wl_1").await;
    n.inner.set_counters(&id, cpu(0, 1 << 20));
    n.sampler.read(t(0)).await;
    n.sampler.read(t(600)).await;
    let pending = n.meter.with_book(|b| b.pending("wl_1"));
    assert_eq!(pending.counters.memory_kib_seconds, Some(1024 * 10 + 1024 * 30));
    assert_eq!(pending.losses[0].cause, sample::MEMORY_UNREAD);
}
