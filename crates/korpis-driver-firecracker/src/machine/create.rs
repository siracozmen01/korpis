// SPDX-License-Identifier: AGPL-3.0-or-later

//! `Create`: the spec checked again whatever `Prepare` said, since the driver can do what root
//! can; then the drives (the shared boot drive, the image's drive built once per image, the
//! object's own config drive) and the jail, and nothing run.

use super::{Machine, MachineError, MachineResult, failed};
use crate::jail::{self, Drives};
use crate::object::Object;
use crate::settings::Settings;
use korpis_image::build::{Request, file_name};
use korpis_proto::korpis::driver::v1 as driver;
use korpis_proto::korpis::v1 as pb;
use std::path::{Path, PathBuf};

/// The ids a workload's range holds.
const IDS: u32 = 65_536;
/// The most secret values a console redacts, and the longest name and value together.
const SECRETS: usize = 4096;
const SECRET: usize = 64 << 10;

fn refused(why: impl Into<String>) -> MachineError {
    MachineError::Refused(why.into())
}

/// That `spec` is one this driver builds: its tier, its image by digest, its ids a whole range
/// above the host's own, no network yet, its console written to its own stream volume alone,
/// and every volume beneath the volume root.
fn validate(spec: &driver::PreparedSpec, settings: &Settings) -> MachineResult<()> {
    if spec.tier.as_known() != Some(pb::IsolationTier::ISOLATION_TIER_MICROVM) {
        return Err(refused("the firecracker driver builds the microvm tier alone"));
    }
    let digest = spec.rootfs.rsplit_once('@').map_or("", |(_, d)| d);
    korpis_core::content::hex_of(digest).map_err(|_| refused("an image by its digest"))?;
    if spec.uid_count != IDS || spec.uid_base < IDS || spec.uid_base.checked_add(IDS).is_none() {
        return Err(refused("a range of 65536 ids above the host's own"));
    }
    if !spec.namespaces.is_empty() {
        return Err(refused("a guest is given no network by this driver yet"));
    }
    if !korpis_driver_kit::console::own_dir(spec, &settings.volume_root) {
        return Err(refused("only the workload's own stream volume is written to"));
    }
    let secrets = spec.stream.as_option().map_or(&[][..], |s| &s.secrets[..]);
    let long = |x: &driver::StreamSecret| x.name.len() + x.value.len() > SECRET;
    if secrets.len() > SECRETS || secrets.iter().any(long) {
        return Err(refused("too many secrets to redact, or one too long"));
    }
    let root = settings.volume_root.canonicalize().map_err(failed)?;
    for m in &spec.mounts {
        // Checked by the name it resolves to, which is the name the guest's drive is bound from.
        let source = Path::new(&m.source).canonicalize().map_err(|e| refused(e.to_string()))?;
        if !source.starts_with(&root) || !source.is_file() || !m.destination.starts_with('/') {
            return Err(refused(format!(
                "volume {:?} is no drive beneath the volume root",
                m.volume
            )));
        }
    }
    Ok(())
}

/// The kernel, copied once among the drives so a jail can hold it as a hard link.
fn kernel(settings: &Settings) -> std::io::Result<PathBuf> {
    let bytes = std::fs::read(&settings.kernel)?;
    let digest = korpis_core::content::digest_of(&bytes);
    let at = settings.drives().join(format!("vmlinux-{}", digest.trim_start_matches("sha256:")));
    if !at.is_file() {
        let tmp = at.with_extension("tmp");
        std::fs::write(&tmp, &bytes)?;
        std::fs::rename(&tmp, &at)?;
    }
    Ok(at)
}

/// The drives every guest of `spec`'s image shares, made if missing, and what the image says
/// about running it.
fn shared(
    settings: &Settings,
    spec: &driver::PreparedSpec,
) -> std::io::Result<(PathBuf, PathBuf, PathBuf, korpis_core::content::image::ImageConfig)> {
    let boot = korpis_image::boot::drive(&settings.guest, &settings.drives())?;
    let source = korpis_image::source::resolve(&settings.content, &spec.rootfs)?;
    let image = settings.drives().join(file_name(&source.manifest));
    if !image.is_file() {
        let request = Request {
            content: settings.content.clone(),
            image: spec.rootfs.clone(),
            out: image.clone(),
            ids: spec.uid_base,
        };
        korpis_image::build::run(&settings.image_build, &request)?;
    }
    Ok((kernel(settings)?, boot, image, source.config))
}

/// Whether `a` and `b` are the same workload's same run of the same intent.
fn same(a: &driver::PreparedSpec, b: &driver::PreparedSpec) -> bool {
    let run = |s: &driver::PreparedSpec| s.labels.as_option().map(|l| l.run.clone());
    a.workload == b.workload && a.intent == b.intent && run(a) == run(b)
}

impl Machine {
    /// Builds what `spec` asks for and runs nothing; idempotent by workload, intent, and run.
    pub async fn create(&self, spec: driver::PreparedSpec) -> MachineResult<String> {
        validate(&spec, self.settings())?;
        let _held = self.lock.lock().await;
        if let Some(o) =
            Object::all(&self.settings.objects()).into_iter().find(|o| same(&o.spec, &spec))
        {
            return Ok(o.id);
        }
        let id = format!("fc-{}", uuid::Uuid::now_v7().simple());
        let dir = self.settings.object(&id);
        let settings = std::sync::Arc::clone(&self.settings);
        let made = {
            let (id, dir) = (id.clone(), dir.clone());
            tokio::task::spawn_blocking(move || build(&settings, &id, &dir, &spec))
        };
        match made.await.map_err(failed)? {
            Ok(()) => Ok(id),
            Err(e) => {
                let _ = jail::remove(&self.settings, &id);
                let _ = std::fs::remove_dir_all(&dir);
                Err(e)
            }
        }
    }
}

/// Object `id`'s record, drives, and jail, in `dir`.
fn build(
    settings: &Settings,
    id: &str,
    dir: &Path,
    spec: &driver::PreparedSpec,
) -> MachineResult<()> {
    Object::create(dir, id, spec).map_err(failed)?;
    let (kernel, boot, image, config) = shared(settings, spec).map_err(failed)?;
    let guest = crate::guest::config(spec, &config).map_err(refused)?;
    if let Some(stream) = spec.stream.as_option() {
        crate::console::prepare(dir, stream).map_err(failed)?;
    }
    let config_drive = Object::config(dir);
    guest.write(&mut std::fs::File::create(&config_drive).map_err(failed)?).map_err(failed)?;
    let owner = Some(spec.uid_base);
    std::os::unix::fs::chown(&config_drive, owner, owner).map_err(failed)?;
    let mut volumes = Vec::new();
    for m in &spec.mounts {
        // Each volume's drive is its guest's to read and write, as its own first id: no other
        // user of the host reads it.
        let source = Path::new(&m.source).canonicalize().map_err(failed)?;
        std::os::unix::fs::lchown(&source, Some(spec.uid_base), Some(spec.uid_base))
            .map_err(failed)?;
        volumes.push((source, m.read_only));
    }
    let drives = Drives { kernel, boot, image, config: config_drive, volumes };
    Object::keep_drives(dir, &drives).map_err(failed)?;
    // Built here as well as at each start, so what would fail to build fails before anything runs.
    jail::build(settings, id, &drives, &crate::guest::shape(spec)).map_err(failed)
}
