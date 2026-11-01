# Stack

What Korpis is written in and built on, derived from where its security boundary is rather than from
taste. Everything Korpis writes is Rust, except what runs in a browser, which is TypeScript.
Processes are still separated by the protocols between them, because the separation exists for
privilege, not for languages. How the crates are layered is in [architecture](architecture.md).

## Contents

- [Components](#components)
- [Why Rust throughout](#why-rust-throughout)
- [Why the boundary could never be Go](#why-the-boundary-could-never-be-go)
- [Async](#async)
- [Drivers](#drivers)
- [The oci driver and containerd](#the-oci-driver-and-containerd)
- [What the microvm tier runs that no crate holds](#what-the-microvm-tier-runs-that-no-crate-holds)
- [Protobuf and Connect](#protobuf-and-connect)
- [TLS and cryptography](#tls-and-cryptography)
- [SSH](#ssh)
- [PostgreSQL](#postgresql)
- [The panel's toolchain](#the-panels-toolchain)
- [Independent counterparts in tests](#independent-counterparts-in-tests)

## Components

| Component | Language | Changes |
|---|---|---|
| Schema and protocol | Protocol Buffers, served over Connect | rarely, compatibly |
| Control plane (`korpis-server`), and the relay as its role | Rust | constantly |
| Agent supervisor | Rust | often |
| Privileged helper | Rust, synchronous, single-threaded | almost never |
| Per-workload filesystem workers | Rust | almost never |
| Guest agent (`korpis-guest`) | Rust, static for musl | rarely |
| Runtime drivers | Rust for every first-party driver, any language for a third party's | independently |
| Panel | TypeScript, static bundle | constantly |
| CLI | Rust, one static binary | often |
| Database | PostgreSQL | |

## Why Rust throughout

**The boundary is most of the agent.** It covers the privileged helper, the filesystem workers, the
guest agent in every microVM, the relay carrying every console and file transfer, and the `oci`
driver, whose validation of incoming specs is the only thing between a compromised supervisor and
root ([security](security.md)). What would be left for another language is the supervisor, whose job
is to produce the specs those components distrust, and the control plane.

**One set of types for what must agree.** An assignment is signed by the control plane and verified
by the agent, a capability token minted by one and verified by the agent and the relay, a lease's
term measured by both ends under rules that hold only if both compute them identically. In one
language each is one crate used on both sides of the wire; in two, each is two implementations that
a conformance test must prove equal, and the first divergence is a security defect. Biscuit's
reference implementation, the token format chosen ([capability tokens](capability-tokens.md)), is
Rust.

**The type system carries the state machines.** Plan steps, leases and epochs, power cycles, journal
cursors and observation states are enums, so an unhandled state is a compile error, and newtypes
make an epoch incomparable with a version and an organization identifier unpassable as a workload's.

**One supply chain.** One toolchain, one dependency tree audited by `cargo-deny`, one reproducible
build, one set of advisories to watch. **No collector in the data path**: the relay forwards
interactive console bytes and the agent samples metering under a buffer bound, with no pause to
reason about.

Connect, gRPC, PostgreSQL tooling and static binaries are as mature in Rust as anywhere. A deep
contributor pool would matter if outside contribution to the core were how Korpis scales; it scales
through conformance suites that do not care what language a conforming agent or driver is written
in.

What it costs:

- **Compile times**, paid on the code that changes most. Mitigated by a workspace of small crates
  with the protocol types at the bottom, so a change to reconciliation logic does not rebuild the
  world.
- **Async Rust** is harder than goroutines: cancellation, pinning and `Send` bounds are real
  complexity.
- **A pre-1.0 Connect implementation** at the root of the API (below).
- **A smaller pool of people who could take the project over**, which succession in
  [GOVERNANCE.md](../../GOVERNANCE.md) has to answer.

## Why the boundary could never be Go

The helper and the filesystem workers hold root or sit inside a workload's namespace, the code paths
whose equivalents produced the largest class of published Wings advisories ([pterodactyl/wings
advisories](https://github.com/pterodactyl/wings/security/advisories)). Go cannot do this part
correctly without leaving Go:

- **Joining a mount namespace is impossible from a Go process.** `setns` into a mount namespace
  fails with `EINVAL` for a thread sharing filesystem attributes (`CLONE_FS`) with another thread,
  and every thread of a Go process does
  ([setns(2)](https://man7.org/linux/man-pages/man2/setns.2.html)). `unshare(CLONE_NEWUSER)` and
  `setns` into a user namespace require a single-threaded caller, and the Go runtime starts its
  threads before `main`.
- **Landlock confines the calling thread** and what it creates afterwards
  ([landlock(7)](https://man7.org/linux/man-pages/man7/landlock.7.html)); a Go process already has
  other threads, and goroutines migrate between them. seccomp alone would not force the choice,
  since `SECCOMP_FILTER_FLAG_TSYNC` applies a filter to every thread.
- `fork` without an immediate `exec` is unsafe in Go, which rules out fork, confine, exec.

`runtime.LockOSThread()` neither makes the process single-threaded nor stops `CLONE_FS` sharing.
runc is Go and does its namespace setup in C, in a constructor (`nsexec.c`) that runs before the Go
runtime starts ([runc
libcontainer/nsenter](https://github.com/opencontainers/runc/tree/main/libcontainer/nsenter)).

Rust does not lift the constraint; it lets a program obey it. A Rust process is single-threaded
until it chooses otherwise, so the helper never starts a second thread and a filesystem worker
confines itself before it serves anything. One language does not merge the processes: folding the
helper into the supervisor to save a socket would merge their privilege domains, which the split
exists to prevent.

## Async

Tokio, never in the privileged helper. The control plane, the relay, the supervisor and the CLI use
it. The helper serves one structured request at a time, synchronously, and does not link an async
runtime: the generated Connect services, which bring Tokio, are a feature of the wire crate the
helper leaves off, and `scripts/check-layers.sh` fails if Tokio reaches it. A filesystem worker
answers its session's requests one at a time on its socket, synchronously, and applies its Landlock
ruleset before serving anything; the relay, not the worker, serves SFTP.

## Drivers

Drivers are separate processes behind a versioned interface ([runtimes](runtimes.md)), so a third
party may write one in any language against the same contract. Every first-party driver is Rust:

| Driver | Talks to |
|---|---|
| `oci` | containerd's gRPC API, generated from containerd's own schema |
| `firecracker` | Firecracker's HTTP API |
| `qemu` | QEMU's machine protocol (QMP), supervising its process |
| `native` | the kernel directly; it is the confinement path |
| `winjob`, `winsilo` | Job Objects and the Host Compute Service through Microsoft's `windows` crate, with C# the fallback if an API proves unreachable; not built |

## The oci driver and containerd

containerd access is root-equivalent, so the driver's refusal of a spec outside the container tier's
invariants is a security boundary, written and reviewed as such. The driver talks to containerd over
its published gRPC API using containerd's own `.proto` files, vendored unchanged in
`crates/korpis-containerd` (whose `proto/README.md` records the tag and digest) and generated by the
same toolchain as Korpis's schema. containerd's Rust client crate was not used: it generates code at
build time with `tonic` and `prost`, needing a protobuf compiler on every build machine and putting
a second protobuf runtime and a second HTTP/2 client into the one process whose validation is the
boundary. The cost is keeping the vendored schema current by hand, rare within a containerd major
version, and implementing what the Go client wraps (image pulling, snapshot preparation, task
management) in a security-critical component.

## What the microvm tier runs that no crate holds

`deny.toml` governs crates, and three things the tier needs are not crates, so each is pinned where
it is fetched:

- **Firecracker and its jailer** are Firecracker's release binaries, pinned in `scripts/tools.sh` by
  the digests the release published, so an asset replaced after the commit is refused.
- **The guest kernel** is built, not downloaded: `scripts/guest-kernel.sh` fetches kernel.org's
  longterm source by digest and builds it with Firecracker's guest configuration, vendored in
  `guest/kernel/` with its source and digest recorded, stamped so the same compiler rebuilds the
  same bytes.
- **The guest's programs**, `korpis-guest`, and `korpis-fsworker` and `korpis-install-job` on its
  boot drive, are Rust built static for the musl target `rust-toolchain.toml` installs, since they
  run on a root filesystem Korpis did not make. The install job decodes zstd with `ruzstd` so it
  needs no C compiler; the reference C decoder only makes its tests' archives.

Firecracker's security releases and the kernel's longterm updates reach Korpis only through a
reviewed commit moving a pin, a chore no tool does. Until a release pipeline builds the kernel, a
node's operator runs the scripts, and `scripts/microvm-files.sh` puts the results where installation
finds them.

## Protobuf and Connect

One schema generates the Rust server and agent, the TypeScript client (`connect-es`), SDKs and the
OpenAPI document ([api](api.md)). Connect serves Connect, gRPC and gRPC-Web on one endpoint, from a
browser without a proxy and from `curl` with JSON; an integration market that needs a protobuf
toolchain to send one request does not form. The Rust implementation is `connectrpc`, which passes
Connect's conformance suite, on the protobuf runtime `buffa`, which implements canonical ProtoJSON.
Intent bodies are stored in that encoding ([state](state.md)), so the encoder writing the database
and the one answering `curl` are the same code.

`connectrpc` is pre-1.0, and the protocol freeze does not wait for its 1.0. What is frozen is the
wire contract: the schema in `crates/korpis-proto` and the Connect, gRPC and gRPC-Web protocols as
their specifications define them, which the conformance suite tests and which are no property of the
crate's Rust API. Every crate pins `connectrpc` and `buffa` exactly, and the domain and application
crates never hold a generated type (`scripts/check-layers.sh`), so a breaking release of the crate
is an adapter update changing no byte on the wire, checked end to end by the API, agent and CLI
tests. The cost is that every upgrade is reviewed by hand. The fallback, serving gRPC and gRPC-Web
from `tonic` and losing plain-JSON access until a replacement exists, is taken only if a release
Korpis needs fails the conformance suite or an advisory against the crate goes unfixed upstream.

## TLS and cryptography

One TLS stack and one provider per process: every TLS connection goes through `rustls`, with `ring`
installed as the process's default provider before anything else runs (the API listener, the
database connection, the certificates `install` mints, and the OCI registry client resolving
recipes). A provider chosen per connection would make "which primitives does this process use" a
question with several answers. The registry client verifies against the host's trust store, so a
private registry behind a private authority is trusted by adding the authority to the host; plain
HTTP is used only for registries named with the control plane's `--plain-http-registry`, which
exists for development, and a private registry on a private network is named with `--fetch-allow`,
since the registry client connects through a proxy that reaches nothing private otherwise.

`connectrpc` depends on `rustls` with default features, which build and link `aws-lc-rs` too, so
every binary carries a provider it never selects and every build compiles its C sources. Removing it
needs the dependency declared without default features upstream; until then it is dead weight, not a
second code path.

## SSH

A relay serving SFTP speaks SSH through `russh`, on Tokio and built with `ring`, and reads SFTP
packets with `russh-sftp`'s codec. The loop answering them is Korpis's own ([files](files.md)): the
library's loop keeps reading after a packet it could not read, from a stream that has lost its
framing, where Korpis's ends the connection. Two costs:

- RSA keys are verified through the `rsa` crate, which
  [RUSTSEC-2023-0071](https://rustsec.org/advisories/RUSTSEC-2023-0071) says leaks its private key
  through timing, with no fixed version. The advisory concerns private-key operations, and a relay
  holds no RSA key (its host key is Ed25519 and any other is refused), so the crate only checks a
  client's signature with a public key; `deny.toml` states that reasoning beside the advisory.
- `russh` checks a client's signature against the algorithm the client named, not the list the
  server offers, so a client signing with `ssh-rsa` (SHA-1) is accepted although the relay offers
  RSA only with SHA-2. OpenSSH's client does not sign so by default, and a forgery needs a SHA-1
  collision covering the session's identifier, but the relay does not refuse one; that takes a
  change to `russh`.

## PostgreSQL

The transactional guarantees that make intents and effects honest are not optional: an effect
written in the transaction of the change it describes is the whole basis of the audit model. SQLite
cannot provide what [state](state.md) requires, and MySQL offers worse JSON and `SKIP LOCKED` for no
gain. The control plane uses `sqlx`, every query checked against the schema at compile time from
`.sqlx/`, so a migration breaking a query fails the build. The cost is that a single operator with
one machine installs a database as a second thing ([install](../user/install.md)).

## The panel's toolchain

| Concern | Choice | Why |
|---|---|---|
| language | TypeScript, `strict`, no `any` outside generated code | the panel's types come from the `.proto`, and a hole in them is where client and server drift |
| build | Vite, static files with hashed names | no server runtime; hashed names let a CDN cache forever |
| API client | Connect-ES with protobuf-es, generated by `buf` from the server's `.proto` | one schema; server streams over `fetch`, no WebSocket |
| components | React and React Aria | accessibility ([panel](panel.md#the-stack)) |
| messages | FormatJS, ICU catalogues extracted from the source and checked against it | one format for core and recipe strings |
| tests | Vitest and Testing Library; Playwright against a real control plane, with its virtual WebAuthn authenticator | a panel tested only against mocks tests the mocks |
| packages | pnpm, exact versions, a frozen committed lockfile, install scripts refused | the npm registry is the project's largest supply-chain surface |

pnpm is pinned in `package.json` at an exact version and digest, verified by corepack. `.npmrc`
refuses install scripts and saves exact versions; esbuild, the one package with a script, is denied
it by name in `pnpm-workspace.yaml`, since the script only checks a binary the build runs anyway.
`scripts/check-web.sh` installs from the frozen lockfile, regenerates the client into a scratch
directory and fails on any difference from `web/src/gen`, type-checks, lints with
typescript-eslint's strict type-checked rules, runs the component tests, builds, and reads every
package's licence from the lockfile against the allow list in `deny.toml`, one list for both.
`caniuse-lite` is excepted by name in `web/licence-exceptions.json`, its CC-BY-4.0 covering
browser-support data the build reads and never ships. Vite stays on 7, because Vite 8 bundles CSS
through `lightningcss` (MPL-2.0, outside the list). A newer Vite, or a dependency moving to a
licence outside the list, waits for a commit arguing it.

The browser tests live in `web/e2e` with their own TypeScript project and lint, and are run by a
test of `korpis-e2e` (`scripts/check.sh full`) that stands up a control plane serving the built
panel on a free port, a node with a workload where one can run (root, beside containerd),
invitations and a waiting `korpis login`, and afterwards checks what only the machine can see (an
uploaded file against the node's copy, the CLI's sign-in). That test is its own binary, run alone,
because the node tests change the machine's network interfaces and Chromium abandons what is in
flight when they do. Two fixtures stand in for time and trust: the harness moves a session's sign-in
back eleven minutes rather than making the ten-minute rule configurable, and Chromium is told to
ignore the control plane's certificate error, without which it registers no service worker.
Playwright's Chromium is installed by `scripts/test-tools.sh` with Playwright's own installer and
verified against no digest pinned here, the one test download trusted on its publisher's word.

## Independent counterparts in tests

Where Korpis implements one side of a protocol, it is tested against an independent implementation
of the other, each in a package of its own under `conformance/` with its own lockfile and held to
the panel's package policy:

- **WebAuthn.** The relying party is Korpis's own: CBOR and COSE decoded by `ciborium` and `coset`,
  signatures verified by `ring`. It is tested against 1Password's `passkey` crate in
  `conformance/webauthn`, kept out of the main workspace because it turns on a `serde_json` feature
  that changes map ordering for every crate's tests.
- **OpenID Connect.** ID tokens are verified by `ring` (RS256, ES256 and EdDSA from the issuer's
  published keys) rather than by a JWT library with a second cryptographic stack and a wider
  algorithm list. It is tested against panva's `oidc-provider`, which is OpenID Certified, in
  `conformance/oidc`: codes with PKCE, ID tokens in each algorithm, and refresh tokens rotated and
  revoked on reuse. Only the issuer's login and consent are scripted. The browser test signs a
  person in through the `korpis-idp-oidc` provider's own binary, run on the node's host in a mount
  namespace placing the workload's identity socket and relay where the workload's mount puts them;
  only the workload's own sandbox around it is left untested.
