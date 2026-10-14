// SPDX-License-Identifier: AGPL-3.0-or-later

//! `korpis backup clone` (§5.4 of docs/06-storage.md): a new workload declared with the body of
//! the workload a backup is of, the backed-up volume created holding what the backup held and
//! every other volume created empty, so the copy runs beside the original. It is how a restore is
//! tested. It is composed of reads and one declaration, which takes `backup.read` on the source
//! as any volume created from a backup does.

use super::{Answer, json, mutation};
use crate::connect::Connection;
use crate::error::CliError;
use crate::show;
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::declare_intent_request::Target;

fn gone(what: &str) -> CliError {
    CliError::Local(format!("{what} is not there any more"))
}

/// The source's body made the clone's: `volume` from `backup`, and every volume it attached
/// created instead, at the size `sizes` gives each.
fn cloned(
    mut body: pb::IntentBody,
    (backup, volume): (&str, &str),
    sizes: &[(String, u64)],
) -> Result<pb::IntentBody, CliError> {
    let mut seeded = false;
    for v in &mut body.volumes {
        if let Some(existing) = v.existing.take() {
            let size = sizes.iter().find(|(id, _)| *id == existing).map_or(0, |(_, s)| *s);
            v.size_bytes = v.size_bytes.max(size);
        }
        if v.name == volume {
            v.from_backup = Some(backup.to_owned());
            seeded = true;
        }
    }
    if seeded { Ok(body) } else { Err(gone(&format!("the workload's volume {volume}"))) }
}

/// Declares workload `name` from backup `backup`, in `project` or the source's.
pub async fn clone(
    c: &Connection,
    backup: &str,
    (name, project): (&str, Option<&str>),
) -> Result<Answer, CliError> {
    let b = pb::BackupServiceClient::new(c.http(), c.config())
        .get_backup(pb::GetBackupRequest { id: backup.to_owned(), ..Default::default() })
        .await?
        .into_owned()
        .backup
        .into_option()
        .ok_or_else(|| gone("the backup"))?;
    let volumes = pb::VolumeServiceClient::new(c.http(), c.config());
    let get = |id: String| pb::GetVolumeRequest { id, ..Default::default() };
    let volume = volumes.get_volume(get(b.volume.clone())).await?.into_owned().volume;
    let volume = volume.into_option().ok_or_else(|| gone("the backed-up volume"))?;
    let status = pb::WorkloadServiceClient::new(c.http(), c.config())
        .get_workload(pb::GetWorkloadRequest { id: b.workload.clone(), ..Default::default() })
        .await?
        .into_owned()
        .status
        .into_option()
        .ok_or_else(|| gone("the workload"))?;
    let source = status.workload.into_option().ok_or_else(|| gone("the workload"))?;
    let body = status.target.into_option().and_then(|t| t.body.into_option());
    let body = body.ok_or_else(|| CliError::Local("the workload has no approved intent".into()))?;
    let mut sizes = Vec::new();
    for id in body.volumes.iter().filter_map(|v| v.existing.clone()) {
        let v = volumes.get_volume(get(id.clone())).await?.into_owned().volume;
        sizes.push((id, v.into_option().map_or(0, |v| v.size_bytes)));
    }
    let body = cloned(body, (&b.id, &volume.name), &sizes)?;
    let create = pb::NewWorkload {
        project: project.map_or(source.project, str::to_owned),
        name: name.to_owned(),
        ..Default::default()
    };
    let r = pb::DeclareIntentRequest {
        mutation: mutation(None).into(),
        target: Some(Target::Create(Box::new(create))),
        body: body.into(),
        ..Default::default()
    };
    let declared = pb::WorkloadServiceClient::new(c.http(), c.config()).declare_intent(r).await?;
    let declared = declared.into_owned();
    let workload = declared.workload.into_option().unwrap_or_default();
    let plan = declared.plan.into_option().unwrap_or_default();
    let human = format!(
        "workload {} from backup {}\n{}",
        workload.id,
        b.id,
        show::plan(&plan, &workload.id)
    );
    Ok(Answer { human, json: json(&plan) })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(name: &str, existing: Option<&str>, size: u64) -> pb::VolumeSpec {
        pb::VolumeSpec {
            name: name.into(),
            existing: existing.map(str::to_owned),
            size_bytes: size,
            ..Default::default()
        }
    }

    #[test]
    fn the_backed_up_volume_is_seeded_and_every_attachment_created_instead() {
        let body = pb::IntentBody {
            volumes: vec![spec("data", None, 10), spec("maps", Some("vol_m"), 0)],
            ..Default::default()
        };
        let sizes = [("vol_m".to_owned(), 5)];
        let c = cloned(body.clone(), ("bkp_1", "data"), &sizes).unwrap();
        assert_eq!(c.volumes[0].from_backup.as_deref(), Some("bkp_1"));
        assert_eq!((c.volumes[1].existing.as_deref(), c.volumes[1].size_bytes), (None, 5));
        assert!(cloned(body, ("bkp_1", "gone"), &sizes).is_err());
    }
}
