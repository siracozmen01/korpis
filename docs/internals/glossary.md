# Glossary

One concept, one name, in code, API and documents. The fields of each object are in the API
schema (`crates/korpis-proto`); how each behaves is on the page linked.

## Contents

- [Tenancy and authority](#tenancy-and-authority)
- [Workloads and state](#workloads-and-state)
- [Placement](#placement)
- [Attached resources](#attached-resources)
- [Words Korpis does not use](#words-korpis-does-not-use)
- [Identity rules](#identity-rules)

## Tenancy and authority

- **Organization**: the root of a tenancy tree, or a node in one. Contains projects and
  sub-organizations, holds a quota set, and is suspended or resumed only from above. A hosting
  provider is a root organization, its customer a child, that customer's customers grandchildren;
  nothing in the code tells these levels apart. See [tenancy](tenancy.md).
- **Project**: a named group of workloads inside an organization, and the usual scope of a grant.
- **Quota set**: what an organization or project may consume, as entries scoped by selector, each
  with reservations (never oversubscribed) and limits (which may exceed the parent's by a declared
  ratio). Named so that it never collides with plan. See [quota](quota.md).
- **Subject**: anything that can act: a user, a token, an external identity (a Discord account, an
  OIDC group member), a workload, an extension. Not necessarily registered with Korpis.
- **Grant**: the only authority primitive. A subject, actions, a scope and conditions; it can only
  produce weaker children, and every effect names the grant that permitted it. See
  [grants](grants.md).
- **Grant template**: a named bundle that expands into grants. A role in the interface, never in
  the model.
- **Identity provider binding**: an organization's trust in an external identity provider, mapping
  its groups or roles to grants.
- **Portal**: a name an organization holds, under which the control plane serves its API and panel.

## Workloads and state

- **Workload**: the stable identity of a thing that runs, to which every other piece of state
  attaches. Deliberately almost empty.
- **Intent**: what someone declared should be true. Immutable and versioned, a chain per workload.
- **Power**: whether a workload should be running, declared apart from its configuration so a
  restart never needs a new intent.
- **Declared set**: the workloads one declarative source owns, such as the files a pipeline applies
  with `korpis apply`.
- **Observation**: what the node's agent actually sees. Written only by the agent, never merged
  with intent.
- **Plan**: the computed difference between a declaration and reality, persisted, inspectable and
  approvable before anything happens. See [state](state.md).
- **Effect**: what actually happened. Append-only; each names its plan and its grant.
- **Operation**: a parent owning many plans, with one approval and one progress view, such as a
  drain.
- **Recipe**: a signed, versioned, content-addressed package describing how to run a class of
  workload. Korpis's package format; there is no other name for it. See [recipes](recipes.md).

## Placement

- **Node**: a machine running the Korpis agent.
- **Placement**: the binding of a workload to a node, held under a lease.
- **Lease**: a node's time-bound, epoch-numbered right to run a workload; a node that cannot renew
  stops it, so two nodes never run one workload. See [reconciliation](reconciliation.md).
- **Assignment**: everything a node is told: which workloads it runs, at which lease epochs, with
  their resolved configuration.
- **Journal**: what a node records durably about what it did, so a restarted agent resumes rather
  than repeats.
- **Migration**: one workload's move from one node to another through prepare, replicate, verify,
  quiesce, cutover, release and the hold before cleanup. See [migration](migration.md).
- **Driver**: the runtime that starts a workload on a node (OCI containers, Firecracker
  microVMs), declaring what it can do and validating every spec it receives. See
  [runtimes](runtimes.md).

## Attached resources

- **Volume**: durable state with a lifecycle independent of the workload using it. See
  [storage](storage.md).
- **Snapshot**: a point a volume can be rolled back to, on the volume's own node and filesystem.
  Not a backup.
- **Repository**: where an organization's backups are kept, and under whose key.
- **Backup**: one backup of one volume, content-addressed and deduplicated, as the node that took
  it reported it. See [backup](backup.md).
- **Endpoint**: a way the network reaches a workload: a port on a node, a stable address, an HTTP
  route, a dedicated IP. See [endpoints](endpoints.md).
- **Domain**: a DNS name an organization has shown it controls, which its endpoints may route.
- **Certificate**: a certificate and key for the names of an endpoint the ingress terminates.
- **Address pool**: addresses an organization's network routes to its nodes, from which its
  workloads hold one each for a dedicated-address endpoint.
- **Edge group**: nodes of an organization that hold the stable addresses of its endpoints, each
  address on one of them at a time, and forward to wherever each workload runs.
- **Stream**: a durable, addressable channel of output from a workload (console, logs), read and
  written by offset. See [streams](streams.md).
- **Extension**: a separate process holding narrow grants that adds to Korpis through the same
  contract first-party features use. See [extensions](extensions.md).

## Words Korpis does not use

- **Server**: ambiguous between the machine and the workload; it is node or workload.
- **Location, nest**: labels and selectors.
- **Allocation**: endpoint.
- **Egg**, and the name of Pterodactyl's extension framework: recipe.
- **Role**, **admin flag**: a grant, a widely scoped grant.
- **spec and status on one object**: intent and observation, with separate writers.
- No game concept appears anywhere: query protocols, RCON, modpacks and wipes are recipe and
  extension concerns.

## Identity rules

- Identifiers are immutable and never reused; names are mutable and may be reused. Nothing internal
  refers to anything by name.
- Deletion is soft at the identity layer: an identifier persists so that effects, streams, backups
  and metering records are never attributed to a different object.
- Content-addressed things (recipes, stream segments, backup chunks) are identified by digest. A
  human-readable name is a pointer resolved at declaration and recorded as a digest.
- No object carries a type field that changes behaviour in core. Behaviour comes from declared
  capabilities: a driver's capabilities, a recipe's preset, an extension's contract version. Core
  asks; it does not switch on kind.
