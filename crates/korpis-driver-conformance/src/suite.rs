// SPDX-License-Identifier: Apache-2.0

//! The suite standing in for the node (§8 of docs/04-runtimes.md): it asks the driver what it
//! is, lays the probe's image into the content store, and makes each object as the agent would:
//! an intent of a recipe naming the probe for the driver's tier, prepared by the driver, with what
//! the agent fills in before `Create` (the labels adoption matches, the workload's ids, and each
//! volume's source, made as the helper makes it). Everything it made is destroyed and removed at
//! the end of the run, whatever the cases found.

use crate::client::Driver;
use crate::fixture::Fixture;
use crate::image;
use crate::volume::{self, Kind};
use korpis_proto::korpis::driver::v1 as driver;
use korpis_proto::korpis::v1 as pb;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

/// The size of each object's volume.
const VOLUME: u64 = 64 << 20;
/// What each object reserves and may use.
const MEMORY: (u64, u64) = (64 << 20, 128 << 20);
/// How many processes each object may run.
const PIDS: u64 = 64;

/// A suite of cases against one driver.
#[derive(Debug)]
pub struct Suite {
    /// What it was given.
    pub fixture: Fixture,
    /// The driver.
    pub driver: Driver,
    /// What the driver says it is.
    pub info: pb::DriverInfo,
    /// The probe's image, by reference.
    pub image: String,
    next: AtomicU32,
    made: Mutex<Vec<(String, String)>>,
}

/// A wire id of `prefix` that no one else holds.
fn fresh(prefix: &str) -> String {
    format!("{prefix}_{}", uuid::Uuid::now_v7().simple())
}

/// A tier as a recipe names it.
fn tier_name(tier: pb::IsolationTier) -> &'static str {
    match tier {
        pb::IsolationTier::ISOLATION_TIER_PROCESS => "process",
        pb::IsolationTier::ISOLATION_TIER_MICROVM => "microvm",
        pb::IsolationTier::ISOLATION_TIER_VM => "vm",
        _ => "container",
    }
}

/// The recipe document of the probe's image `image` as a `tier` target, running `command`.
#[must_use]
pub fn recipe(tier: pb::IsolationTier, image: &str, command: &[&str]) -> String {
    let command = serde_json::to_string(command).unwrap_or_default();
    format!(
        r#"apiVersion: korpis.dev/v1
kind: Recipe
metadata: {{name: conformance-probe, version: "1.0.0", description: the probe, license: Apache-2.0}}
runtime:
  targets:
    - {{tier: {tier}, arch: [{arch}], image: "{image}"}}
  entry:
    command: {command}
preset:
  lifecycle: persistent
  console: none
  volumes:
    - {{name: data, path: /data, size_default: 64Mi}}
  resources:
    memory: {{min: 64Mi, recommended: 128Mi}}
    cpu: {{min: 100m, recommended: 500m}}
"#,
        tier = tier_name(tier),
        arch = image::arch(),
    )
}

/// An intent of workload `workload` for `driver` at `tier`, with one volume, `data`.
#[must_use]
pub fn intent(workload: &str, (driver, tier): (&str, pb::IsolationTier)) -> pb::Intent {
    let reserved =
        |reservation, limit| pb::ReservedLimit { reservation, limit, ..Default::default() };
    let resources = pb::ResourceSpec {
        cpu: pb::CpuSpec { reservation_millis: 100, ..Default::default() }.into(),
        memory: reserved(MEMORY.0, MEMORY.1).into(),
        pids: pb::PidsSpec { limit: PIDS, ..Default::default() }.into(),
        ..Default::default()
    };
    let volume = pb::VolumeSpec {
        name: "data".into(),
        path: "/data".into(),
        size_bytes: VOLUME,
        mode: pb::VolumeMode::VOLUME_MODE_RW.into(),
        ..Default::default()
    };
    pb::Intent {
        id: fresh("int"),
        workload: workload.to_owned(),
        version: 1,
        body: pb::IntentBody {
            lifecycle: pb::Lifecycle::LIFECYCLE_PERSISTENT.into(),
            console: pb::ConsoleMode::CONSOLE_MODE_NONE.into(),
            runtime: pb::RuntimeSpec {
                driver: driver.to_owned(),
                tier: tier.into(),
                ..Default::default()
            }
            .into(),
            resources: resources.into(),
            volumes: vec![volume],
            ..Default::default()
        }
        .into(),
        ..Default::default()
    }
}

impl Suite {
    /// The suite against the driver `fixture` names: described, and its probe's image laid.
    ///
    /// # Errors
    /// The driver did not describe itself, or the image could not be laid.
    pub async fn new(fixture: Fixture) -> Result<Self, String> {
        let driver = Driver::connect(&fixture.socket);
        let info = driver.describe().await.map_err(|e| format!("Describe: {e}"))?;
        let image = image::lay(&fixture.content, &fixture.probe)
            .map_err(|e| format!("the probe's image: {e}"))?;
        Ok(Self { fixture, driver, info, image, next: AtomicU32::new(0), made: Mutex::default() })
    }

    /// The driver's declared capabilities.
    #[must_use]
    pub fn capabilities(&self) -> pb::Capabilities {
        self.info.capabilities.as_option().cloned().unwrap_or_default()
    }

    /// The tier the suite runs its objects at: the first the driver declares.
    #[must_use]
    pub fn tier(&self) -> pb::IsolationTier {
        let tiers = self.capabilities().tiers;
        tiers.first().and_then(buffa::enumeration::EnumValue::as_known).unwrap_or_default()
    }

    /// How the driver takes volumes.
    #[must_use]
    pub fn volume_kind(&self) -> Kind {
        let block = pb::VolumeSemantics::VOLUME_SEMANTICS_BLOCK_DEVICE;
        if self.capabilities().volume_semantics.as_known() == Some(block) {
            Kind::Block(VOLUME)
        } else {
            Kind::Files
        }
    }

    /// A new workload's id and its intent, for this driver at its tier.
    #[must_use]
    pub fn workload(&self) -> (String, pb::Intent) {
        let workload = fresh("wl");
        let intent = intent(&workload, (&self.info.name, self.tier()));
        (workload, intent)
    }

    /// The recipe document of the probe, at the driver's tier, running `command`.
    #[must_use]
    pub fn recipe(&self, command: &[&str]) -> Vec<u8> {
        recipe(self.tier(), &self.image, command).into_bytes()
    }

    /// The spec of a new object running the probe's `command`: prepared by the driver, and
    /// completed as the agent completes it.
    ///
    /// # Errors
    /// The driver refused to prepare it, or a volume could not be made.
    pub async fn prepared(&self, command: &[&str]) -> Result<driver::PreparedSpec, String> {
        use driver::prepare_response::Result as R;
        let (workload, intent) = self.workload();
        let answer = self.driver.prepare(&workload, &intent, &self.recipe(command)).await;
        let mut spec = match answer.map_err(|e| format!("Prepare: {e}"))?.result {
            Some(R::Prepared(spec)) => *spec,
            Some(R::Rejection(r)) => return Err(format!("Prepare rejected it: {}", r.detail)),
            None => return Err("Prepare answered nothing".into()),
        };
        let n = self.next.fetch_add(1, Ordering::Relaxed);
        let (base, count) = self.fixture.ids(n).ok_or("the fixture has no more id ranges")?;
        (spec.uid_base, spec.uid_count) = (base, count);
        spec.labels = driver::RuntimeLabels {
            workload: workload.clone(),
            intent: intent.id.clone(),
            assignment_generation: 1,
            lease_epoch: 1,
            ..Default::default()
        }
        .into();
        for m in &mut spec.mounts {
            let name = format!("{workload}.{}", m.volume);
            let made = volume::make(&self.fixture.volume_root, &name, self.volume_kind(), base);
            m.source = made.map_err(|e| format!("volume {name}: {e}"))?.display().to_string();
            self.keep(String::new(), name);
        }
        Ok(spec)
    }

    /// A new object running the probe's `command`, created: its id and its spec.
    ///
    /// # Errors
    /// It could not be prepared, or the driver refused to create it.
    pub async fn created(
        &self,
        command: &[&str],
    ) -> Result<(String, driver::PreparedSpec), String> {
        let spec = self.prepared(command).await?;
        let id = self.driver.create(&spec).await.map_err(|e| format!("Create: {e}"))?;
        self.keep(id.clone(), String::new());
        Ok((id, spec))
    }

    /// A new object running the probe's `command`, started and seen running: its id and spec.
    ///
    /// # Errors
    /// It could not be created or started, or was not seen running.
    pub async fn running(
        &self,
        command: &[&str],
    ) -> Result<(String, driver::PreparedSpec), String> {
        let (id, spec) = self.created(command).await?;
        self.driver.start(&id).await.map_err(|e| format!("Start: {e}"))?;
        self.until(&id, Duration::from_secs(120), |s| state(s) == RUNNING).await?;
        Ok((id, spec))
    }

    /// Keeps an object's id, or a volume's name, to be cleaned away at the end.
    fn keep(&self, id: String, volume: String) {
        if let Ok(mut made) = self.made.lock() {
            made.push((id, volume));
        }
    }

    /// Inspects `id` until `wanted` holds of its status, for at most `within`: the status.
    ///
    /// # Errors
    /// It never held, saying the last status seen.
    pub async fn until(
        &self,
        id: &str,
        within: Duration,
        wanted: impl Fn(&driver::RuntimeStatus) -> bool,
    ) -> Result<driver::RuntimeStatus, String> {
        let deadline = Instant::now() + within;
        loop {
            let status = self.driver.inspect(id).await.map_err(|e| format!("Inspect: {e}"))?;
            if wanted(&status) {
                return Ok(status);
            }
            if Instant::now() >= deadline {
                return Err(format!("after {within:?} it was still {status:?}"));
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// Destroys every object the suite made and removes every volume, whatever state each is in.
    pub async fn clean(&self) {
        let made = self.made.lock().map(|mut m| std::mem::take(&mut *m)).unwrap_or_default();
        for (id, _) in made.iter().filter(|(id, _)| !id.is_empty()) {
            let _ = self.driver.kill(id).await;
            let _ = self.driver.destroy(id).await;
        }
        for (_, name) in made.iter().filter(|(_, v)| !v.is_empty()) {
            let _ = volume::remove(&self.fixture.volume_root, name);
        }
    }
}

/// A running object's state.
pub const RUNNING: Option<driver::RuntimeState> = Some(driver::RuntimeState::RUNTIME_STATE_RUNNING);
/// A stopped object's state.
pub const STOPPED: Option<driver::RuntimeState> = Some(driver::RuntimeState::RUNTIME_STATE_STOPPED);
/// A created object's state.
pub const CREATED: Option<driver::RuntimeState> = Some(driver::RuntimeState::RUNTIME_STATE_CREATED);

/// The state `status` says.
#[must_use]
pub fn state(status: &driver::RuntimeStatus) -> Option<driver::RuntimeState> {
    status.state.as_known()
}
