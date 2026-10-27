// SPDX-License-Identifier: AGPL-3.0-or-later

//! A [`Host`] that keeps its records in memory, for tests of the agent and of what drives it.

use super::{Host, HostError, SnapshotOp};
use korpis_proto::korpis::helper::v1 as helper;
use korpis_proto::korpis::v1 as pb;
use std::collections::{BTreeMap, BTreeSet};
use std::future::{Future, ready};
use std::sync::{Arc, Mutex};

mod files;
mod finish;
mod knobs;

#[derive(Debug, Default)]
struct Kept {
    volumes: BTreeMap<String, String>,
    content: BTreeSet<String>,
    /// Each artifact's files, as a test made them available.
    files: BTreeMap<String, BTreeMap<String, String>>,
    /// Content held, by digest, as a test offered it.
    blobs: BTreeMap<String, Vec<u8>>,
    ids: BTreeMap<String, (u32, u32)>,
    /// The volumes ensured as block devices.
    blocks: BTreeSet<String>,
    snapshots: BTreeSet<(String, String)>,
    jobs: BTreeMap<String, helper::JobStatusResponse>,
    /// What each job was started with.
    started: BTreeMap<String, helper::StartJobRequest>,
    /// The networks asked for, by workload; none is made, so none has a namespace.
    networks: BTreeMap<String, helper::EnsureNetworkRequest>,
    /// The socket the ingress proxy was last started to connect to.
    ingress: Option<std::path::PathBuf>,
    /// How many times it was started.
    ingress_starts: u32,
    /// Where each volume a file session names is served from, as a directory of its own.
    file_root: Option<std::path::PathBuf>,
    /// The file sessions opened.
    opened_files: Vec<helper::OpenFilesRequest>,
    /// What each volume's filesystem accounts, as a test set it; unaccounted where unset.
    usage: BTreeMap<String, u64>,
    /// What the host's table has counted, as a test set it.
    counters: Vec<helper::Counter>,
    /// Each set of CPUs the node's dedicated cores were said to hold.
    held_cores: Vec<helper::HoldCoresRequest>,
    /// Each side of the mesh the node was kept to.
    overlays: Vec<helper::KeepOverlayRequest>,
}

/// A helper and content store that keep their records in memory; clones share them.
#[derive(Debug, Clone, Default)]
pub struct InMemoryHost {
    kept: Arc<Mutex<Kept>>,
}

impl InMemoryHost {
    /// Nothing kept.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn with<T>(&self, f: impl FnOnce(&mut Kept) -> T) -> Result<T, HostError> {
        let mut kept = self.kept.lock().map_err(|_| HostError("poisoned".into()))?;
        Ok(f(&mut kept))
    }
}

impl Host for InMemoryHost {
    fn ensure_volume(
        &self,
        request: &helper::EnsureVolumeRequest,
    ) -> impl Future<Output = Result<helper::EnsureVolumeResponse, HostError>> + Send {
        let volume = request.volume.clone();
        ready(self.with(|k| {
            if request.block_device {
                k.blocks.insert(volume.clone());
            }
            let created = !k.volumes.contains_key(&volume);
            let path = k.volumes.entry(volume.clone()).or_insert(format!("/memory/{volume}"));
            helper::EnsureVolumeResponse {
                host_path: path.clone(),
                created,
                storage_class: request.storage_class.clone(),
                ..Default::default()
            }
        }))
    }

    fn delete_volume(&self, volume: &str) -> impl Future<Output = Result<(), HostError>> + Send {
        ready(self.with(|k| {
            k.volumes.remove(volume);
        }))
    }

    fn snapshot(
        &self,
        op: SnapshotOp,
        volume: &str,
        snapshot: &str,
    ) -> impl Future<Output = Result<(), HostError>> + Send {
        let key = (volume.to_owned(), snapshot.to_owned());
        ready(
            self.with(|k| match op {
                SnapshotOp::Take if k.volumes.contains_key(volume) => {
                    k.snapshots.insert(key);
                    Ok(())
                }
                SnapshotOp::Rollback if k.snapshots.contains(&key) => Ok(()),
                SnapshotOp::Delete => {
                    k.snapshots.remove(&key);
                    Ok(())
                }
                _ => Err(HostError(format!("no volume {volume} with snapshot {snapshot}"))),
            })
            .and_then(|r| r),
        )
    }

    fn fetch(&self, reference: &str) -> impl Future<Output = Result<(), HostError>> + Send {
        ready(self.with(|k| {
            k.content.insert(reference.to_owned());
        }))
    }

    fn fetch_files(
        &self,
        reference: &str,
    ) -> impl Future<Output = Result<BTreeMap<String, String>, HostError>> + Send {
        let found = self.with(|k| {
            k.content.insert(reference.to_owned());
            k.files.get(reference).cloned().unwrap_or_default()
        });
        ready(found)
    }

    fn content_size(
        &self,
        digest: &str,
    ) -> impl Future<Output = Result<Option<u64>, HostError>> + Send {
        let held = self.with(|k| k.blobs.get(digest).map(Vec::len)).ok().flatten();
        ready(Ok(held.map(|n| u64::try_from(n).unwrap_or(u64::MAX))))
    }

    fn read_content(
        &self,
        digest: &str,
        limit: usize,
    ) -> impl Future<Output = Result<Vec<u8>, HostError>> + Send {
        let held = self.with(|k| k.blobs.get(digest).cloned()).ok().flatten();
        ready(match held {
            Some(b) if b.len() <= limit => Ok(b),
            Some(_) => Err(HostError(format!("{digest} is over {limit} bytes"))),
            None => Err(HostError(format!("{digest} is not held"))),
        })
    }

    fn fetch_url(
        &self,
        _url: &str,
        digest: &str,
    ) -> impl Future<Output = Result<(), HostError>> + Send {
        ready(self.with(|k| {
            k.content.insert(digest.to_owned());
        }))
    }

    fn ensure_ids(
        &self,
        workload: &str,
        base: u32,
        count: u32,
    ) -> impl Future<Output = Result<(), HostError>> + Send {
        ready(self.with(|k| {
            k.ids.insert(workload.to_owned(), (base, count));
        }))
    }

    fn storage(&self) -> impl Future<Output = Result<Vec<pb::StorageClassInfo>, HostError>> + Send {
        ready(Ok(Vec::new()))
    }

    /// A job finishes as it starts: a backup with a record, a restore verified.
    fn start_job(
        &self,
        request: &helper::StartJobRequest,
    ) -> impl Future<Output = Result<bool, HostError>> + Send {
        ready(self.with(|k| {
            let finished = finish::at_once(request, &k.blocks);
            let started = !k.jobs.contains_key(&request.job);
            k.jobs.entry(request.job.clone()).or_insert(finished);
            k.started.insert(request.job.clone(), request.clone());
            started
        }))
    }

    fn job_status(
        &self,
        job: &str,
    ) -> impl Future<Output = Result<helper::JobStatusResponse, HostError>> + Send {
        ready(self.with(|k| {
            k.jobs.get(job).cloned().unwrap_or_else(|| helper::JobStatusResponse {
                state: helper::JobState::JOB_STATE_UNKNOWN.into(),
                ..Default::default()
            })
        }))
    }

    fn forget_job(&self, job: &str) -> impl Future<Output = Result<(), HostError>> + Send {
        ready(self.with(|k| {
            k.jobs.remove(job);
        }))
    }

    fn commit_job(&self, job: &str) -> impl Future<Output = Result<(), HostError>> + Send {
        let committed = self.with(|k| {
            let restored = k.jobs.get(job).and_then(|s| s.result.as_option());
            match restored.and_then(|r| r.restored.as_option()) {
                Some(r) if r.verified => Ok(()),
                _ => Err(HostError(format!("job {job} is no whole restore"))),
            }
        });
        ready(committed.and_then(|r| r))
    }

    fn list_jobs(&self) -> impl Future<Output = Result<Vec<helper::JobSummary>, HostError>> + Send {
        ready(self.with(|k| {
            let summary = |(job, s): (&String, &helper::JobStatusResponse)| helper::JobSummary {
                job: job.clone(),
                state: s.state,
                ..Default::default()
            };
            k.jobs.iter().map(summary).collect()
        }))
    }

    fn ensure_network(
        &self,
        request: &helper::EnsureNetworkRequest,
    ) -> impl Future<Output = Result<helper::EnsureNetworkResponse, HostError>> + Send {
        ready(self.with(|k| {
            k.networks.insert(request.workload.clone(), request.clone());
            helper::EnsureNetworkResponse::default()
        }))
    }

    fn release_network(
        &self,
        workload: &str,
    ) -> impl Future<Output = Result<bool, HostError>> + Send {
        ready(self.with(|k| k.networks.remove(workload).is_some()))
    }

    fn open_resolver(
        &self,
        _workload: &str,
    ) -> impl Future<Output = Result<Vec<std::os::fd::OwnedFd>, HostError>> + Send {
        ready(Err(HostError("the in-memory host makes no networks".into())))
    }

    /// A socket of the agent's own namespace, the in-memory host having no other.
    fn open_probe(
        &self,
        _workload: &str,
        udp: bool,
    ) -> impl Future<Output = Result<std::os::fd::OwnedFd, HostError>> + Send {
        use rustix::net::{AddressFamily, SocketFlags, SocketType};
        let kind = if udp { SocketType::DGRAM } else { SocketType::STREAM };
        let opened =
            rustix::net::socket_with(AddressFamily::INET, kind, SocketFlags::CLOEXEC, None);
        ready(opened.map_err(|e| HostError(e.to_string())))
    }

    /// Listeners on loopback in place of each address's ports 80 and 443, and sockets in place
    /// of its UDP port 443, so a test reaches what the agent serves on them.
    fn open_ingress(
        &self,
        addresses: &[String],
    ) -> impl Future<Output = Result<Vec<std::os::fd::OwnedFd>, HostError>> + Send {
        let tcp = (0..addresses.len() * 2)
            .map(|_| std::net::TcpListener::bind("127.0.0.1:0").map(std::os::fd::OwnedFd::from));
        let udp = (0..addresses.len())
            .map(|_| std::net::UdpSocket::bind("127.0.0.1:0").map(std::os::fd::OwnedFd::from));
        let opened = tcp.chain(udp).map(|r| r.map_err(|e| HostError(e.to_string()))).collect();
        ready(opened)
    }

    /// Records `control`, where a test then plays the proxy.
    fn start_ingress(
        &self,
        control: &std::path::Path,
    ) -> impl Future<Output = Result<helper::StartIngressResponse, HostError>> + Send {
        let recorded = self.with(|k| {
            k.ingress = Some(control.to_path_buf());
            k.ingress_starts += 1;
        });
        ready(recorded.map(|()| helper::StartIngressResponse::default()))
    }

    fn stop_ingress(&self) -> impl Future<Output = Result<bool, HostError>> + Send {
        ready(self.with(|k| k.ingress.take().is_some()))
    }

    fn open_files(
        &self,
        request: &helper::OpenFilesRequest,
    ) -> impl Future<Output = Result<std::os::fd::OwnedFd, HostError>> + Send {
        ready(self.serve_files(request))
    }

    fn volume_usage(
        &self,
        volume: &str,
    ) -> impl Future<Output = Result<helper::VolumeUsageResponse, HostError>> + Send {
        ready(self.with(|k| {
            let used = k.usage.get(volume).copied();
            helper::VolumeUsageResponse {
                used_bytes: used.unwrap_or(0),
                accounted: used.is_some(),
                ..Default::default()
            }
        }))
    }

    fn describe_network(
        &self,
    ) -> impl Future<Output = Result<helper::DescribeNetworkResponse, HostError>> + Send {
        ready(Ok(helper::DescribeNetworkResponse {
            why_not: "the in-memory host makes no networks".into(),
            why_not_shapes: "the in-memory host shapes nothing".into(),
            ..Default::default()
        }))
    }

    fn ensure_uplinks(
        &self,
        _request: &helper::EnsureUplinksRequest,
    ) -> impl Future<Output = Result<helper::EnsureUplinksResponse, HostError>> + Send {
        ready(Err(HostError("the in-memory host shapes nothing".into())))
    }

    fn read_counters(
        &self,
    ) -> impl Future<Output = Result<helper::ReadCountersResponse, HostError>> + Send {
        ready(self.with(|k| helper::ReadCountersResponse {
            counters: k.counters.clone(),
            ..Default::default()
        }))
    }

    fn hold_cores(
        &self,
        request: &helper::HoldCoresRequest,
    ) -> impl Future<Output = Result<helper::HoldCoresResponse, HostError>> + Send {
        ready(self.with(|k| {
            k.held_cores.push(request.clone());
            helper::HoldCoresResponse::default()
        }))
    }

    fn keep_overlay(
        &self,
        request: &helper::KeepOverlayRequest,
    ) -> impl Future<Output = Result<helper::KeepOverlayResponse, HostError>> + Send {
        ready(self.with(|k| {
            k.overlays.push(request.clone());
            helper::KeepOverlayResponse { public_key: vec![7; 32], ..Default::default() }
        }))
    }
}
