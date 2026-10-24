// SPDX-License-Identifier: AGPL-3.0-or-later

//! A path of a block device's backup restored into its volume (§5.4 of docs/06-storage.md): the
//! plan's restore step staged the backup's image beside the volume while the workload ran, and
//! this step, once the workload is stopped, has the workload's driver copy the path from that
//! image into a new directory of the volume, in a service guest holding both, as it runs an
//! install. Each pass asks the driver where the copy is, starts it when the driver knows none,
//! and waits while it runs; a copy the driver lost is started again, which finds its own finished
//! work and is done. The staged image is the restore step's job's, which the node forgets once
//! the plan has left the set.

use super::act::Failure;
use super::jobs::job_of;
use super::step::{After, detail};
use super::{Reconciler, Tracked, text};
use crate::host::Host;
use crate::runtime::Runtime;
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::driver::v1 as driver;
use korpis_proto::korpis::install::v1 as install;
use korpis_proto::korpis::v1 as pb;

fn failed(e: impl std::fmt::Display) -> Failure {
    Failure(e.to_string())
}

impl<R: Runtime, H: Host> Reconciler<R, H> {
    /// Moves apply step `s` on; what it restored once the copy ended, `None` while it has not.
    pub(super) async fn apply_step(
        &mut self,
        a: &agent::Assignment,
        s: &pb::Step,
        t: &mut Tracked,
    ) -> Result<Option<After>, Failure> {
        let staged = job_of(s).ok_or_else(|| failed("an apply step names its restore's job"))?;
        let copy = format!("apl-{}", detail(s, "job").unwrap_or_default());
        let mut spec = self.prepare(a, t).await?;
        let name = spec.driver.clone();
        let status = self.runtime.install_status(&name, &copy).await?;
        match status.state.as_known() {
            Some(driver::InstallState::INSTALL_STATE_RUNNING) => Ok(None),
            Some(driver::InstallState::INSTALL_STATE_FINISHED) => {
                let result = status.result.into_option().unwrap_or_default();
                self.runtime.forget_install(&name, &copy).await?;
                if !result.ok {
                    return Err(failed(format!("the restore's copy failed: {}", result.error)));
                }
                let mut after = After::new();
                after.insert("restored".into(), text(detail(s, "into").unwrap_or_default()));
                Ok(Some(after))
            }
            Some(driver::InstallState::INSTALL_STATE_LOST) => {
                self.runtime.forget_install(&name, &copy).await?;
                Ok(None)
            }
            _ => {
                let image = self.host.job_status(&staged).await?.image;
                if image.is_empty() {
                    return Err(failed("the restore step staged no image"));
                }
                self.mount(a, &mut spec).await?;
                (spec.uid_base, spec.uid_count) = (a.uid_base, a.uid_count);
                let asked = copying(s, (&copy, spec), image)?;
                self.runtime.start_install(&asked).await?;
                tracing::info!(job = %copy, "a backup's path is being restored in a guest");
                Ok(None)
            }
        }
    }
}

/// The driver's request for copy `job` of step `s`'s path into the volume of the workload `spec`
/// describes, its volumes placed, from the staged `image`.
fn copying(
    s: &pb::Step,
    (job, spec): (&str, driver::PreparedSpec),
    image: String,
) -> Result<driver::StartInstallRequest, Failure> {
    let volume = detail(s, "volume").unwrap_or_default();
    let at = spec.mounts.iter().find(|m| m.volume == volume).map(|m| m.destination.clone());
    let at = at.ok_or_else(|| failed(format!("the workload mounts no volume {volume}")))?;
    let staged = driver::StagedImage {
        image,
        workload: spec.workload.clone(),
        uid_base: spec.uid_base,
        uid_count: spec.uid_count,
        filesystem: detail(s, "image_filesystem").unwrap_or_default().to_owned(),
        ..Default::default()
    };
    let tree = install::RestoreTree {
        from: detail(s, "path").unwrap_or_default().to_owned(),
        volume: at,
        into: detail(s, "into").unwrap_or_default().to_owned(),
        ..Default::default()
    };
    let mut asked = super::install::for_driver(
        job,
        spec,
        (Vec::new(), 0),
        std::collections::BTreeMap::default(),
        Vec::new(),
    );
    asked.restore_image = staged.into();
    asked.restore = tree.into();
    Ok(asked)
}
