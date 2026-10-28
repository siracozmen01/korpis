// SPDX-License-Identifier: AGPL-3.0-or-later

//! `WorkloadService`'s migrations (§8.1 of docs/05-scheduling.md): proposing one, reading a
//! workload's, and ending one.

use super::super::wire::Refuse;
use super::super::wire::migration::migration_to_pb;
use super::super::wire::plan::plan_to_pb;
use super::super::{Api, encode, error, mutation_key, respond, wire};
use buffa::Message as _;
use connectrpc::{ConnectError, RequestContext, Response, ServiceRequest, ServiceResult};
use korpis_app::workload::{EndMigration, MigrateWorkload, MigrationView};
use korpis_app::{Idempotent, Reply};
use korpis_core::id::IdKind;
use korpis_core::workload::Plan;
use korpis_proto::korpis::v1 as pb;

fn proposed(p: &Plan) -> pb::MigrateWorkloadResponse {
    pb::MigrateWorkloadResponse { plan: plan_to_pb(p).into(), ..Default::default() }
}

fn shown((m, volumes): &MigrationView) -> pb::Migration {
    migration_to_pb(m, volumes)
}

pub(super) async fn propose(
    api: &Api,
    ctx: RequestContext,
    request: ServiceRequest<'_, pb::MigrateWorkloadRequest>,
) -> ServiceResult<pb::MigrateWorkloadResponse> {
    let caller = api.caller(&ctx).await?;
    let req = request.to_owned_message();
    let key = mutation_key(req.mutation.as_option())?;
    let node = match req.node.as_str() {
        "" => None,
        n => Some(wire::id(IdKind::Node, "node", n).refuse()?),
    };
    let hold = req.hold.as_option().map(|d| wire::duration_from("hold", d)).transpose().refuse()?;
    let cmd = MigrateWorkload {
        workload: wire::id(IdKind::Workload, "workload", &req.workload).refuse()?,
        node,
        hold,
    };
    let encoder = |p: &Plan| encode(&proposed(p));
    let idem = Idempotent { key, encode: &encoder };
    let reply = api.app.migrate_workload(&caller, &idem, cmd).await.map_err(error::from_app)?;
    respond(reply, proposed)
}

pub(super) async fn list(
    api: &Api,
    ctx: RequestContext,
    request: ServiceRequest<'_, pb::ListMigrationsRequest>,
) -> ServiceResult<pb::ListMigrationsResponse> {
    let caller = api.caller(&ctx).await?;
    let workload = wire::id(IdKind::Workload, "workload", request.workload).refuse()?;
    let found = api.app.migrations(&caller, workload).await.map_err(error::from_app)?;
    Response::ok(pb::ListMigrationsResponse {
        migrations: found.iter().map(shown).collect(),
        ..Default::default()
    })
}

/// How a migration is ended.
#[derive(Clone, Copy)]
pub(super) enum End {
    Abandon,
    Rollback,
    Release,
}

/// Ends the migration `id` as `how` says, for `reason`, under the key of `mutation`.
pub(super) async fn end(
    api: &Api,
    ctx: RequestContext,
    (mutation, id, reason): (Option<&pb::Mutation>, &str, &str),
    how: End,
) -> Result<pb::Migration, ConnectError> {
    let caller = api.caller(&ctx).await?;
    let key = mutation_key(mutation)?;
    let migration = wire::id(IdKind::Migration, "migration", id).refuse()?;
    let encoder = |v: &MigrationView| encode(&shown(v));
    let idem = Idempotent { key, encode: &encoder };
    let cmd = EndMigration { migration, reason: reason.to_owned() };
    let reply = match how {
        End::Abandon => api.app.abandon_migration(&caller, &idem, cmd).await,
        End::Rollback => api.app.rollback_migration(&caller, &idem, cmd).await,
        End::Release => api.app.release_migration(&caller, &idem, migration).await,
    };
    match reply.map_err(error::from_app)? {
        Reply::Fresh(v) => Ok(shown(&v)),
        Reply::Replayed(bytes) => {
            pb::Migration::decode_from_slice(&bytes).map_err(|e| error::internal(&e))
        }
    }
}
