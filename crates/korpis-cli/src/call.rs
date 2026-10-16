// SPDX-License-Identifier: AGPL-3.0-or-later

//! `korpis call`: every method of the client API, by name, with its request as JSON and its
//! answer printed as the API's own JSON (§1 and §2 of docs/13-surface-cli.md). This is how
//! "every API operation has a command" is kept mechanically rather than by care: [`METHODS`] is
//! the table below, and a test fails the build when the schema has a method the table lacks.
//! The friendlier commands are built over the same calls.

use crate::connect::Connection;
use crate::error::CliError;
use buffa_descriptor::DescriptorPool;
use korpis_proto::korpis::v1 as pb;
use serde_json::Value;

/// The package every client service is in.
const PACKAGE: &str = "korpis.v1";

/// Parses `json` as the request type.
fn parse<T: serde::de::DeserializeOwned>(json: &Value) -> Result<T, CliError> {
    serde_json::from_value(json.clone()).map_err(|e| CliError::Local(format!("the request: {e}")))
}

/// A message as JSON.
fn render<T: serde::Serialize>(message: &T) -> Result<Value, CliError> {
    serde_json::to_value(message).map_err(|e| CliError::Local(format!("the answer: {e}")))
}

macro_rules! table {
    ($( $name:literal => $client:ident :: $call:ident ( $req:ident ) $( stream $resp:ident )? ; )*) => {
        /// Every method this CLI calls, as `korpis.v1.Service/Method`.
        pub const METHODS: &[&str] = &[$($name),*];

        async fn dispatch(
            c: &Connection,
            name: &str,
            request: &Value,
            out: &mut dyn FnMut(Value),
        ) -> Result<(), CliError> {
            match name {
                $( $name => table!(@call c, $client, $call, $req, request, out $(, $resp)?), )*
                other => Err(CliError::Local(format!("no method {other:?}; see `korpis call --list`"))),
            }
        }
    };
    (@call $c:ident, $client:ident, $call:ident, $req:ident, $request:ident, $out:ident) => {{
        let request: pb::$req = parse($request)?;
        let answer = pb::$client::new($c.http(), $c.config()).$call(request).await?;
        $out(render(&answer.into_owned())?);
        Ok(())
    }};
    (@call $c:ident, $client:ident, $call:ident, $req:ident, $request:ident, $out:ident, $resp:ident) => {{
        let request: pb::$req = parse($request)?;
        let mut stream = pb::$client::new($c.http(), $c.config()).$call(request).await?;
        while let Some(m) = stream.message::<pb::$resp>().await? {
            $out(render(&m.to_owned_message())?);
        }
        Ok(())
    }};
}

table! {
    "korpis.v1.ServerInfoService/GetServerInfo" => ServerInfoServiceClient::get_server_info(GetServerInfoRequest);
    "korpis.v1.AccountService/GetInvitation" => AccountServiceClient::get_invitation(GetInvitationRequest);
    "korpis.v1.AccountService/BeginRedemption" => AccountServiceClient::begin_redemption(BeginRedemptionRequest);
    "korpis.v1.AccountService/RedeemWithPasskey" => AccountServiceClient::redeem_with_passkey(RedeemWithPasskeyRequest);
    "korpis.v1.AccountService/OfferTotp" => AccountServiceClient::offer_totp(OfferTotpRequest);
    "korpis.v1.AccountService/RedeemWithPassword" => AccountServiceClient::redeem_with_password(RedeemWithPasswordRequest);
    "korpis.v1.AccountService/BeginSignIn" => AccountServiceClient::begin_sign_in(BeginSignInRequest);
    "korpis.v1.AccountService/SignInWithPasskey" => AccountServiceClient::sign_in_with_passkey(SignInWithPasskeyRequest);
    "korpis.v1.AccountService/SignInWithPassword" => AccountServiceClient::sign_in_with_password(SignInWithPasswordRequest);
    "korpis.v1.SessionService/Exchange" => SessionServiceClient::exchange(ExchangeRequest);
    "korpis.v1.SessionService/SignOut" => SessionServiceClient::sign_out(SignOutRequest);
    "korpis.v1.SessionService/ListSessions" => SessionServiceClient::list_sessions(ListSessionsRequest);
    "korpis.v1.SessionService/EndSession" => SessionServiceClient::end_session(EndSessionRequest);
    "korpis.v1.SessionService/EndOtherSessions" => SessionServiceClient::end_other_sessions(EndOtherSessionsRequest);
    "korpis.v1.ProfileService/GetProfile" => ProfileServiceClient::get_profile(GetProfileRequest);
    "korpis.v1.ProfileService/ReauthenticateWithPasskey" => ProfileServiceClient::reauthenticate_with_passkey(SignInWithPasskeyRequest);
    "korpis.v1.ProfileService/ReauthenticateWithPassword" => ProfileServiceClient::reauthenticate_with_password(ReauthenticateWithPasswordRequest);
    "korpis.v1.ProfileService/BeginAddPasskey" => ProfileServiceClient::begin_add_passkey(BeginAddPasskeyRequest);
    "korpis.v1.ProfileService/AddPasskey" => ProfileServiceClient::add_passkey(AddPasskeyRequest);
    "korpis.v1.ProfileService/RemovePasskey" => ProfileServiceClient::remove_passkey(RemovePasskeyRequest);
    "korpis.v1.ProfileService/SetPassword" => ProfileServiceClient::set_password(SetPasswordRequest);
    "korpis.v1.ProfileService/RemovePassword" => ProfileServiceClient::remove_password(RemovePasswordRequest);
    "korpis.v1.ProfileService/EnrolTotp" => ProfileServiceClient::enrol_totp(EnrolTotpRequest);
    "korpis.v1.ProfileService/RemoveTotp" => ProfileServiceClient::remove_totp(RemoveTotpRequest);
    "korpis.v1.ProfileService/RenewRecoveryCodes" => ProfileServiceClient::renew_recovery_codes(RenewRecoveryCodesRequest);
    "korpis.v1.OrganizationService/CreateOrganization" => OrganizationServiceClient::create_organization(CreateOrganizationRequest);
    "korpis.v1.OrganizationService/GetOrganization" => OrganizationServiceClient::get_organization(GetOrganizationRequest);
    "korpis.v1.OrganizationService/ListOrganizations" => OrganizationServiceClient::list_organizations(ListOrganizationsRequest);
    "korpis.v1.OrganizationService/SetQuota" => OrganizationServiceClient::set_quota(SetQuotaRequest);
    "korpis.v1.OrganizationService/SuspendOrganization" => OrganizationServiceClient::suspend_organization(SuspendOrganizationRequest);
    "korpis.v1.OrganizationService/ResumeOrganization" => OrganizationServiceClient::resume_organization(ResumeOrganizationRequest);
    "korpis.v1.OrganizationService/GetTrustPolicy" => OrganizationServiceClient::get_trust_policy(GetTrustPolicyRequest);
    "korpis.v1.OrganizationService/SetTrustPolicy" => OrganizationServiceClient::set_trust_policy(SetTrustPolicyRequest);
    "korpis.v1.ProjectService/CreateProject" => ProjectServiceClient::create_project(CreateProjectRequest);
    "korpis.v1.ProjectService/GetProject" => ProjectServiceClient::get_project(GetProjectRequest);
    "korpis.v1.ProjectService/ListProjects" => ProjectServiceClient::list_projects(ListProjectsRequest);
    "korpis.v1.WorkloadService/DeclareIntent" => WorkloadServiceClient::declare_intent(DeclareIntentRequest);
    "korpis.v1.WorkloadService/Start" => WorkloadServiceClient::start(StartRequest);
    "korpis.v1.WorkloadService/Stop" => WorkloadServiceClient::stop(StopRequest);
    "korpis.v1.WorkloadService/Restart" => WorkloadServiceClient::restart(RestartRequest);
    "korpis.v1.WorkloadService/PlaceWorkload" => WorkloadServiceClient::place_workload(PlaceWorkloadRequest);
    "korpis.v1.WorkloadService/SetClassification" => WorkloadServiceClient::set_classification(SetClassificationRequest);
    "korpis.v1.WorkloadService/DeleteWorkload" => WorkloadServiceClient::delete_workload(DeleteWorkloadRequest);
    "korpis.v1.WorkloadService/GetWorkload" => WorkloadServiceClient::get_workload(GetWorkloadRequest);
    "korpis.v1.WorkloadService/ListWorkloads" => WorkloadServiceClient::list_workloads(ListWorkloadsRequest);
    "korpis.v1.WorkloadService/ListIntents" => WorkloadServiceClient::list_intents(ListIntentsRequest);
    "korpis.v1.WorkloadService/WatchWorkload" => WorkloadServiceClient::watch_workload(WatchWorkloadRequest) stream WatchWorkloadResponse;
    "korpis.v1.ExtensionService/InstallExtension" => ExtensionServiceClient::install_extension(InstallExtensionRequest);
    "korpis.v1.ExtensionService/GetExtension" => ExtensionServiceClient::get_extension(GetExtensionRequest);
    "korpis.v1.ExtensionService/ListExtensions" => ExtensionServiceClient::list_extensions(ListExtensionsRequest);
    "korpis.v1.ExtensionService/UninstallExtension" => ExtensionServiceClient::uninstall_extension(UninstallExtensionRequest);
    "korpis.v1.ExtensionService/UpgradeExtension" => ExtensionServiceClient::upgrade_extension(UpgradeExtensionRequest);
    "korpis.v1.ProviderService/ListProviders" => ProviderServiceClient::list_providers(ListProvidersRequest);
    "korpis.v1.ProviderService/PingProvider" => ProviderServiceClient::ping_provider(PingProviderRequest);
    "korpis.v1.EventService/CreateSubscription" => EventServiceClient::create_subscription(CreateSubscriptionRequest);
    "korpis.v1.EventService/GetSubscription" => EventServiceClient::get_subscription(GetSubscriptionRequest);
    "korpis.v1.EventService/ListSubscriptions" => EventServiceClient::list_subscriptions(ListSubscriptionsRequest);
    "korpis.v1.EventService/DeleteSubscription" => EventServiceClient::delete_subscription(DeleteSubscriptionRequest);
    "korpis.v1.EventService/Subscribe" => EventServiceClient::subscribe(SubscribeRequest) stream SubscribeResponse;
    "korpis.v1.EventService/Ack" => EventServiceClient::ack(AckRequest);
    "korpis.v1.DeclaredSetService/ListDeclaredSets" => DeclaredSetServiceClient::list_declared_sets(ListDeclaredSetsRequest);
    "korpis.v1.DeclaredSetService/SetDeclaredSetMode" => DeclaredSetServiceClient::set_declared_set_mode(SetDeclaredSetModeRequest);
    "korpis.v1.PlanService/GetPlan" => PlanServiceClient::get_plan(GetPlanRequest);
    "korpis.v1.PlanService/ListPlans" => PlanServiceClient::list_plans(ListPlansRequest);
    "korpis.v1.PlanService/ApprovePlan" => PlanServiceClient::approve_plan(ApprovePlanRequest);
    "korpis.v1.PlanService/RejectPlan" => PlanServiceClient::reject_plan(RejectPlanRequest);
    "korpis.v1.PlanService/CancelPlan" => PlanServiceClient::cancel_plan(CancelPlanRequest);
    "korpis.v1.EffectService/ListEffects" => EffectServiceClient::list_effects(ListEffectsRequest);
    "korpis.v1.NodeService/CreateEnrollmentToken" => NodeServiceClient::create_enrollment_token(CreateEnrollmentTokenRequest);
    "korpis.v1.NodeService/GetNode" => NodeServiceClient::get_node(GetNodeRequest);
    "korpis.v1.NodeService/ListNodes" => NodeServiceClient::list_nodes(ListNodesRequest);
    "korpis.v1.NodeService/CordonNode" => NodeServiceClient::cordon_node(CordonNodeRequest);
    "korpis.v1.NodeService/UncordonNode" => NodeServiceClient::uncordon_node(UncordonNodeRequest);
    "korpis.v1.NodeService/RemoveNode" => NodeServiceClient::remove_node(RemoveNodeRequest);
    "korpis.v1.NodeService/CreateRelayEnrollmentToken" => NodeServiceClient::create_relay_enrollment_token(CreateRelayEnrollmentTokenRequest);
    "korpis.v1.NodeService/ListRelays" => NodeServiceClient::list_relays(ListRelaysRequest);
    "korpis.v1.NodeService/RemoveRelay" => NodeServiceClient::remove_relay(RemoveRelayRequest);
    "korpis.v1.GrantService/IssueGrant" => GrantServiceClient::issue_grant(IssueGrantRequest);
    "korpis.v1.GrantService/GetGrant" => GrantServiceClient::get_grant(GetGrantRequest);
    "korpis.v1.GrantService/ListGrants" => GrantServiceClient::list_grants(ListGrantsRequest);
    "korpis.v1.GrantService/RevokeGrant" => GrantServiceClient::revoke_grant(RevokeGrantRequest);
    "korpis.v1.GrantService/MintToken" => GrantServiceClient::mint_token(MintTokenRequest);
    "korpis.v1.StreamService/GetStream" => StreamServiceClient::get_stream(GetStreamRequest);
    "korpis.v1.StreamService/ReadOffloaded" => StreamServiceClient::read_offloaded(ReadOffloadedRequest) stream ConsoleChunk;
    "korpis.v1.VolumeService/GetVolume" => VolumeServiceClient::get_volume(GetVolumeRequest);
    "korpis.v1.VolumeService/ListVolumes" => VolumeServiceClient::list_volumes(ListVolumesRequest);
    "korpis.v1.VolumeService/DeleteVolume" => VolumeServiceClient::delete_volume(DeleteVolumeRequest);
    "korpis.v1.VolumeService/CreateSnapshot" => VolumeServiceClient::create_snapshot(CreateSnapshotRequest);
    "korpis.v1.VolumeService/ListSnapshots" => VolumeServiceClient::list_snapshots(ListSnapshotsRequest);
    "korpis.v1.VolumeService/RollbackVolume" => VolumeServiceClient::rollback_volume(RollbackVolumeRequest);
    "korpis.v1.VolumeService/DeleteSnapshot" => VolumeServiceClient::delete_snapshot(DeleteSnapshotRequest);
    "korpis.v1.RepositoryService/CreateRepository" => RepositoryServiceClient::create_repository(CreateRepositoryRequest);
    "korpis.v1.RepositoryService/GetRepository" => RepositoryServiceClient::get_repository(GetRepositoryRequest);
    "korpis.v1.RepositoryService/ListRepositories" => RepositoryServiceClient::list_repositories(ListRepositoriesRequest);
    "korpis.v1.CertificateService/CreateCertificate" => CertificateServiceClient::create_certificate(CreateCertificateRequest);
    "korpis.v1.CertificateService/ReplaceCertificate" => CertificateServiceClient::replace_certificate(ReplaceCertificateRequest);
    "korpis.v1.CertificateService/GetCertificate" => CertificateServiceClient::get_certificate(GetCertificateRequest);
    "korpis.v1.CertificateService/ListCertificates" => CertificateServiceClient::list_certificates(ListCertificatesRequest);
    "korpis.v1.CertificateService/DeleteCertificate" => CertificateServiceClient::delete_certificate(DeleteCertificateRequest);
    "korpis.v1.DomainService/CreateDomain" => DomainServiceClient::create_domain(CreateDomainRequest);
    "korpis.v1.DomainService/VerifyDomain" => DomainServiceClient::verify_domain(VerifyDomainRequest);
    "korpis.v1.DomainService/GetDomain" => DomainServiceClient::get_domain(GetDomainRequest);
    "korpis.v1.DomainService/ListDomains" => DomainServiceClient::list_domains(ListDomainsRequest);
    "korpis.v1.DomainService/DeleteDomain" => DomainServiceClient::delete_domain(DeleteDomainRequest);
    "korpis.v1.BackupService/CreateBackup" => BackupServiceClient::create_backup(CreateBackupRequest);
    "korpis.v1.BackupService/GetBackup" => BackupServiceClient::get_backup(GetBackupRequest);
    "korpis.v1.BackupService/ListBackups" => BackupServiceClient::list_backups(ListBackupsRequest);
    "korpis.v1.BackupService/RestoreBackup" => BackupServiceClient::restore_backup(RestoreBackupRequest);
    "korpis.v1.BackupService/BrowseBackup" => BackupServiceClient::browse_backup(BrowseBackupRequest);
    "korpis.v1.BackupService/PruneBackups" => BackupServiceClient::prune_backups(PruneBackupsRequest);
    "korpis.v1.BackupService/ReadBackupFile" => BackupServiceClient::read_backup_file(ReadBackupFileRequest) stream ReadBackupFileResponse;
}

/// `name` in full: `Service/Method` is read in the client package.
#[must_use]
pub fn full_name(name: &str) -> String {
    if name.starts_with(&format!("{PACKAGE}.")) {
        name.to_owned()
    } else {
        format!("{PACKAGE}.{name}")
    }
}

/// The schema's own record of the method `name`'s request type.
fn input_type(pool: &DescriptorPool, name: &str) -> Option<String> {
    let (service, method) = name.split_once('/')?;
    let m = pool.service_by_name(service)?.method(method)?;
    Some(pool.message(m.input()).full_name().to_owned())
}

/// Gives a mutating request the idempotency key it lacks: a fresh one per invocation, so a
/// retry the CLI makes is safe and two invocations are two requests (§6 of docs/10-api.md).
fn with_idempotency(name: &str, mut request: Value) -> Value {
    let Ok(pool) = DescriptorPool::decode(korpis_proto::DESCRIPTOR_SET) else { return request };
    let mutating = input_type(&pool, name)
        .and_then(|t| pool.message_by_name(&t).map(|m| m.field_by_name("mutation").is_some()))
        .unwrap_or(false);
    let Some(object) = request.as_object_mut().filter(|_| mutating) else { return request };
    let mutation =
        object.entry("mutation").or_insert_with(|| Value::Object(serde_json::Map::default()));
    if let Some(m) = mutation.as_object_mut()
        && !m.contains_key("idempotencyKey")
        && !m.contains_key("idempotency_key")
    {
        m.insert("idempotencyKey".into(), Value::String(uuid::Uuid::now_v7().to_string()));
    }
    request
}

/// Calls the method `name` with `request`, sending each answer to `out`.
pub async fn call(
    c: &Connection,
    name: &str,
    request: Value,
    out: &mut dyn FnMut(Value),
) -> Result<(), CliError> {
    let name = full_name(name);
    let request = with_idempotency(&name, request);
    dispatch(c, &name, &request, out).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_method_of_the_client_api_has_a_command() {
        let pool = DescriptorPool::decode(korpis_proto::DESCRIPTOR_SET).unwrap();
        let schema: std::collections::BTreeSet<String> = pool
            .services()
            .iter()
            .filter(|s| s.full_name().starts_with(&format!("{PACKAGE}.")))
            .flat_map(|s| {
                // A method the client streams to (a provider's Serve) is served by a program
                // holding a connection, not typed at a terminal.
                s.methods()
                    .iter()
                    .filter(|m| !m.is_client_streaming())
                    .map(move |m| format!("{}/{}", s.full_name(), m.name()))
            })
            .collect();
        let table: std::collections::BTreeSet<String> =
            METHODS.iter().map(|m| (*m).to_owned()).collect();
        assert_eq!(schema, table, "every API method has a command, and no command is private");
    }

    #[test]
    fn a_mutation_gets_a_key_and_a_read_gets_nothing() {
        let declare = with_idempotency("korpis.v1.PlanService/ApprovePlan", serde_json::json!({}));
        assert!(declare["mutation"]["idempotencyKey"].is_string());
        let kept = serde_json::json!({ "mutation": { "idempotencyKey": "mine" } });
        assert_eq!(with_idempotency("korpis.v1.PlanService/ApprovePlan", kept.clone()), kept);
        let read = with_idempotency("korpis.v1.PlanService/GetPlan", serde_json::json!({}));
        assert_eq!(read, serde_json::json!({}));
        assert_eq!(full_name("PlanService/GetPlan"), "korpis.v1.PlanService/GetPlan");
    }
}
