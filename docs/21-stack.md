# Implementation Stack

**Status:** design **Date:** 2026-08-07, **revised** 2026-10-12 **Depends on:**
[`17-security.md`](./17-security.md), [`02-architecture.md`](./02-architecture.md),
[`10-api.md`](./10-api.md) **Implements:** Principle P3

> Revised 2026-10-12. The first version of this document split Korpis between Go (the control
> plane, the agent supervisor, the CLI, the `oci` driver) and Rust (the privileged helper and the
> filesystem workers). Question 6 reopened that split after the second external review, and the
> maintainer decided it: **Rust throughout**, with TypeScript for the web client only. §3 gives the
> reasoning, including why the arguments the first version made for Go no longer hold. §4, the case
> for Rust at the boundary, is unchanged, because it was never in dispute.

---

## 1. Why this document exists, and why it comes last

Language and framework choices were deliberately deferred until the constraints were fixed.
Choosing a stack first is choosing the shape of the system by accident: a project that picks a
language before it knows where its security boundary is will draw the boundary where the language
is comfortable.

Now the boundary is known (§6 of `17-security.md`), so the stack can be derived from it rather than
from taste.

**One decision governs everything below: everything Korpis writes is Rust, except what runs in a
browser.** The architecture still separates components into processes speaking defined protocols
(§6 of `02-architecture.md`, §4.3 of `04-runtimes.md`), and that separation is unchanged: it exists
to separate privilege, not languages. What changed is the conclusion drawn from it. The first
version argued that because the components are separate processes, they need not share a language.
That is true and it is not a reason to use two. The question is which language is best for each
component, and once the boundary turned out to cover most of the agent (§3), the answer stopped
differing by component.

---

## 2. Decisions

| Component | Language | Size | Changes |
|---|---|---|---|
| Schema and protocol | **Protocol Buffers**, served over **Connect** | none | rarely, additively |
| Control plane (`korpis-server`) | **Rust** | largest | constantly |
| Relay (a role of `korpis-server`, deployable on its own) | **Rust** | small | rarely |
| Agent supervisor | **Rust** | medium | often |
| **Privileged helper** | **Rust**, synchronous, single-threaded | tiny | almost never |
| **Per-workload filesystem workers** | **Rust** | small | almost never |
| **Guest agent** (`korpis-guest`) | **Rust** | small | rarely |
| Runtime drivers | **Rust** for every first-party driver; any language for a third party's | small each | independently |
| Web client | **TypeScript**, static bundle | medium | constantly |
| CLI | **Rust**, one static binary | small | often |
| Database | **PostgreSQL** | none | none |

---

## 3. Rust throughout, and why

### What the first version argued, and why it no longer holds

The first version chose Go for the control plane on five grounds. Each was checked again on
2026-10-12.

- **"Connect and gRPC are first-class in Go."** They are first-class in Rust now as well. Connect
  has an official Rust implementation, `connectrpc`, adopted into the Connect project and passing
  its full conformance suite, serving Connect, gRPC, and gRPC-Web on one endpoint exactly as §3 of
  `10-api.md` requires. It is pre-1.0, which is a real cost and is carried as question 7 rather than
  waved away; question 7 is resolved, and says what the cost is.
- **"The PostgreSQL tooling is mature."** So is Rust's: `sqlx` checks every query against the schema
  at compile time, and `tokio-postgres` is long-established.
- **"A single static binary."** Rust produces one as readily as Go does.
- **"Compile times keep iteration fast on the codebase that changes most."** True, and still a cost
  (below). It was the one argument that stands, and on its own it does not outweigh what follows.
- **"The contributor pool is deep."** That argument assumed a governance model in which outside
  contribution to the control plane is how the project scales. Korpis has one maintainer (§6 of
  `19-governance.md`), and the contribution gate that scales is the conformance suites (§8 of the
  same document), which test behaviour through published protocols and do not care what language
  a conforming agent or driver is written in (K-7). The pool that matters for the core is the
  people who will maintain it, and that is decided by the maintainer, not by the market.

### Why Rust is better, not merely acceptable

**The boundary grew until it was most of the agent.** The first version called the Rust surface "a
few thousand lines" because it counted only the helper and the filesystem workers. The second
external review drew the boundary where it actually is: the per-workload filesystem workers, the
guest agent inside every microVM, the relay that carries every console and file transfer, and the
`oci` driver, whose validation of incoming specs is the only thing between a compromised supervisor
and root (§6.1 of `02-architecture.md`). What was left in Go was the supervisor and the control
plane, and the supervisor's job is to produce the specs those components have to distrust.

**One set of types for the things that must agree.** An assignment is signed by the control plane
and verified by the agent (§4.8 of `02-architecture.md`); a capability token is minted by the
control plane and verified by the agent and the relay (§6 of `08-identity.md`); a lease's term is
measured by both ends under rules that only hold if both ends compute them identically (§4.5 of
`02-architecture.md`). In one language, each of these is one crate used on both sides of the wire.
In two, each is two implementations that a conformance test has to prove equal, and the first
divergence is a security defect. Biscuit's reference implementation, the token format chosen in §10
of `08-identity.md`, is itself Rust.

**The type system carries the state machines.** The model is made of state machines: plan steps,
leases and their epochs, power cycles, journal cursors, observation states. Rust's enums make an
unhandled state a compile error, and its newtypes make an epoch impossible to compare with a version
or an organization identifier impossible to pass where a workload identifier was expected. Those are
exactly the defect classes that the walkthroughs and both external reviews kept finding on paper; in
code, the compiler finds them.

**One supply chain, one audit.** One toolchain, one dependency tree audited with `cargo-deny` and
`cargo-vet`, one reproducible-build pipeline, one set of security advisories to watch. For a project
with one maintainer, that halving is not cosmetic.

**No collector in the data path.** The relay forwards interactive console bytes, and the agent
samples metering under a buffer bound (§2 of `15-observability.md`). Neither has a pause that an
operator has to reason about.

### What it costs, stated

- **Compile times.** Real, and paid on the codebase that changes most. Mitigated by structure rather
  than accepted as fate: a Cargo workspace of small crates with the protocol types at the bottom, so
  a change to reconciliation logic does not rebuild the world. The crates follow a
  ports-and-adapters layering (a pure domain crate, an application crate of use cases and ports,
  and adapters for PostgreSQL, the API, and the runtime), described with its dependency rule in
  `ARCHITECTURE.md` at the repository root.
- **Async Rust is harder than goroutines.** Cancellation, pinning, and `Send` bounds are genuine
  complexity. The control plane, the relay, and the supervisor use Tokio; the privileged helper uses
  no async runtime at all (question 2).
- **A pre-1.0 Connect implementation** at the root of the API (question 7).
- **A smaller pool of people who could take the project over.** This is the cost the succession
  document of §6 of `19-governance.md` has to answer, and it is stated there.

---

## 4. Rust where the boundary is, and this one was never a preference

The privileged helper and the per-workload filesystem workers are the two components that hold root
or sit inside a workload's namespace. They are also, precisely, the code paths whose equivalents
produced **the largest single class of published Wings advisories** (§1.1 of
`research/evidence.md`). This section was written to justify a second language at the boundary of a
Go system. It is kept because it is why the boundary could never have been Go, which is the half of
the decision that did not depend on the maintainer.

**Small and security-critical.** A few thousand lines each, rarely changed, doing exactly the work
where a memory-safety bug is a tenant escape. This is the ideal shape for Rust even before the
second argument.

**Go cannot do this part correctly without leaving Go.** This is a concrete technical constraint,
not an aesthetic one:

- **Joining a mount namespace is impossible from a Go process, not merely racy.** `setns` into a
  mount namespace fails with `EINVAL` for a thread that shares filesystem attributes (`CLONE_FS`)
  with any other thread, and every thread of a Go process does. `unshare(CLONE_NEWUSER)` and
  `setns` into a user namespace require the caller to be single-threaded, which a Go process never
  is: the runtime starts its threads before `main` runs.
- **Landlock rulesets confine the calling thread** and what it creates afterwards. A Go process has
  other threads already running, which the ruleset does not cover, and goroutines migrate between
  threads, so "this code runs confined" cannot be stated. `seccomp` is different, and the first
  version of this list was wrong to group it here: `SECCOMP_FILTER_FLAG_TSYNC` applies a filter to
  every thread at once, so seccomp alone would not force the choice. Mount namespaces, user
  namespaces, and Landlock do.
- `fork` without immediate `exec` is unsafe in Go, which rules out the classic fork → confine →
  exec pattern that is the natural way to build these processes.

> Second external review, finding S77. Recorded in §17 of `23-walkthroughs.md`. The first version
> called the namespace problem a race. It is a hard error, which makes the argument stronger than it
> was written, and it grouped seccomp with the per-thread mechanisms, which made it weaker than it
> looked to anyone who knew about `TSYNC`.

`runtime.LockOSThread()` does not help with the first point at all, since it does not make the
process single-threaded or stop the other threads sharing `CLONE_FS`. The proof is in the field:
**runc is written in Go and performs its namespace setup in C**, in a constructor (`nsexec.c`) that
runs before the Go runtime initializes, because there was no correct way to do it from Go.

**Rust does not lift the constraint; it lets a program obey it.** A Rust process running Tokio's
multi-threaded scheduler cannot join a mount namespace either. The difference is that a Rust
program is single-threaded until it chooses otherwise, so the ordering the kernel demands can be
written down and kept: the helper never starts a second thread, and a filesystem worker confines
itself before it starts its runtime (question 2). `openat2(RESOLVE_BENEATH)`, Landlock, and seccomp
are all directly expressible with precise control over which thread is confined and when, which is
the mechanism §4 of `17-security.md` names as load-bearing.

**Rust throughout does not merge the processes.** One language makes it tempting to fold the helper
into the supervisor and save a socket. That would merge their privilege domains, which is what the
split exists to prevent (question 1). The process boundaries of §6 of `02-architecture.md` are the
security design, and the language decision leaves every one of them where it was.

---

## 5. Drivers

Drivers are separate processes with a versioned interface (§4.3 of `04-runtimes.md`), so a third
party may write one in any language and conform to the same contract (K-7). Every first-party
driver is Rust:

| Driver | Talks to | Note |
|---|---|---|
| `oci` | containerd, over its gRPC API, generated from containerd's own schema (§6) | the driver's spec validation is security-critical (§6.1 of `02-architecture.md`) |
| `firecracker` | Firecracker's HTTP API | Firecracker is itself Rust |
| `qemu` | QEMU's machine protocol (QMP) | spawns and supervises a process |
| `native` | the kernel directly | it *is* the confinement path, the argument of §4 |
| `winjob` / `winsilo` | Job Objects and the Host Compute Service | through Microsoft's `windows` crate; carried by question 3 |

This is still the payoff for the driver interface being a protocol rather than a plugin ABI: the
first-party drivers share a language, and nothing requires anyone else's to.

---

## 6. The `oci` driver, which the first version left in Go

The first version kept `oci` in Go because containerd's ecosystem is Go, and admitted that this put
security-critical code outside the boundary language: containerd access is root-equivalent, so the
driver's refusal of a spec outside the container tier's invariants is the only thing between a
compromised supervisor and root (§6 of `17-security.md`). That tension is now gone. The driver talks
to containerd over containerd's published gRPC API, which is how any client does, and its validation
is Rust like the rest of the boundary, held to the helper's review standard (§6.1 of
`02-architecture.md`).

It is not free. The Go containerd client wraps that API in conveniences (image pulling, snapshot
preparation, task management) that the Rust client does not, so the driver implements those itself
against the raw API. That is more code in a security-critical component, and it is written and
reviewed as such.

> Corrected 2026-10-13, from the implementation. The table above named containerd's own Rust
> client crate (`containerd-client`). That crate generates its code at build time with `tonic` and
> `prost`, which needs a protobuf compiler on every build machine and puts a second protobuf runtime
> and a second HTTP/2 client into the one process whose validation is the boundary, beside the
> `buffa` and `connectrpc` every other Korpis program uses. containerd's schema is published and
> Apache-2.0, and `connectrpc` speaks gRPC over a Unix socket, so the driver uses containerd's
> `.proto` files themselves, vendored unchanged at the version they were taken from and generated
> by the same toolchain as Korpis's own schema (`crates/korpis-containerd`, whose
> `proto/README.md` records the tag and digest). The cost is keeping the vendored schema current
> by hand, which containerd's backward compatibility within a major version makes a rare chore,
> and owning what the client crate would have wrapped; the second point was already true, since
> the Rust client never had the Go client's conveniences.

Worth noting and not worth weighting: Wings is Go, and Calagopus is Rust. Neither fact influenced
this decision in either version.

---

## 7. Protobuf and Connect, restated

Settled in §3 of `10-api.md`. One schema generates the Rust server and agent, the TypeScript client
(`connect-es`), SDKs in whatever else is wanted, and the OpenAPI document. Connect gives Connect,
gRPC, and gRPC-Web on one endpoint, reachable from a browser without a proxy and from `curl` with
JSON.

On the Rust side the protobuf runtime is `buffa`, which `connectrpc` is built on and which
implements the canonical ProtoJSON mapping. That matters beyond the API: §3.1 of `03-state.md`
stores intent bodies in their canonical JSON encoding, so the encoder that writes the database and
the encoder that answers a `curl` request are the same code.

The JSON surface being first-class is the ecosystem decision, not a convenience: an integration
market that requires a protobuf toolchain to send one request does not form.

**One TLS stack and one cryptographic provider per process.** Every TLS connection Korpis makes or
accepts goes through `rustls`, with `ring` installed as the process's default provider before
anything else runs: the API listener, the database connection, the certificates `install` mints,
and the OCI registry client that resolves recipes (§2 of `09-recipes.md`). A provider chosen per
connection would make "which primitives does this process use" a question with several answers.
The registry client verifies certificates against the host's trust store, so a private registry
behind a private authority is trusted by adding that authority to the host; plain HTTP is used only
for the registries named with `--plain-http-registry`, which exists for development. A private
registry on a private network is also named with `--fetch-allow`, because the registry client
connects through a proxy that reaches nothing private otherwise (§3 of `17-security.md`).

> Corrected 2026-10-15, and withdrawn the same day. The correction changed `--plain-http-registry`
> here to `--plain-http`, reading this as the node agent's flag. It is the control plane's, whose
> registry client resolves recipes, and the control plane's flag is `--plain-http-registry`; the
> node agent and the CLI take `--plain-http` (§3 of `18-operations.md`). This sentence was right.

What it costs: `connectrpc` depends on `rustls` with its default features, which build and link
`aws-lc-rs` as well, so every binary carries a second provider it never selects and every build
compiles its C sources. Removing it needs that dependency declared without default features
upstream; until then it is dead weight, not a second code path.

**SSH, for SFTP.** A relay that serves SFTP (§3.1 of `02-architecture.md`) speaks SSH through
`russh`, which runs on Tokio and is built with `ring`, as the rest of the process is, and reads SFTP
packets with `russh-sftp`'s codec. The loop that answers them is Korpis's own (§3.2 of
`06-storage.md`): the library's loop goes on reading after a packet it could not read, from a
stream that has lost its framing, where Korpis's ends the connection. What it costs, twice over.
SSH keys of RSA are verified through the `rsa` crate, which RUSTSEC-2023-0071 says leaks its private
key through timing, with no fixed version; the advisory concerns operations with a private key, and
a relay holds none of RSA, since its host key is Ed25519 and any other is refused, so the crate only
checks a client's signature with a public one, and `deny.toml` states that reasoning beside the
advisory rather than silencing it. And `russh` checks a client's signature against the algorithm
the client named, not against the list the server offers, so a client that signs with `ssh-rsa`,
whose hash is SHA-1, is accepted although the relay offers RSA only with SHA-2. OpenSSH's client
does not sign so by default, and a forgery would need a SHA-1 collision that covers the session's
own identifier, but the relay does not refuse one; it would take a change to `russh` to.

---

## 8. PostgreSQL only, and the friction it creates

Settled in §6 of `03-state.md` and restated because it is the stack's one genuine tension.

The transactional guarantees that make `Intent` and `Effect` honest are not optional, an `Effect`
written in the same transaction as the state change it describes is the entire basis of the audit
model. SQLite cannot provide what §4 of `03-state.md` requires, and MySQL's `jsonb`-equivalent and
`SKIP LOCKED` story is worse for no gain.

The control plane reaches it through `sqlx`, with every query checked against the schema at compile
time, so a migration that breaks a query fails the build rather than a request.

The cost is that §1 of `18-operations.md` promises a single operator with one machine can install
Korpis, and a database is a second thing to install. §2 of `18-operations.md` states this plainly
rather than hiding it, and §10's first open question is about reducing the friction without
shipping an unsupervised production configuration nobody tests.

---

## 9. Web client

TypeScript, compiled to a static bundle with no server runtime (§1 of `11-surface-web.md`). The
framework was left open until the panel was built, with the constraints that would decide it
fixed: static output, streams rather than polling, correct rendering for a subject holding an
arbitrary grant set, and small enough that a host serving it from a cheap CDN is ordinary.

**Decided 2026-10-16: React, with React Aria for accessible primitives**, for the reasons and at
the cost given in question 1 of §10 of `11-surface-web.md`. The rest of the toolchain:

| Concern | Choice | Why |
|---|---|---|
| language | TypeScript, `strict`, no `any` outside generated code | the panel's types come from the `.proto`, and a hole in them is where the client and the server drift |
| build | Vite, to static files with hashed names | static output with no server runtime is the constraint; hashed names let a CDN cache them forever |
| API client | Connect-ES with protobuf-es, generated by `buf` from the same `.proto` as the server | one schema, three languages' clients (§3 of `10-api.md`); server streams over `fetch`, no WebSocket |
| messages | FormatJS (`react-intl`, its CLI, and its lint rules), ICU MessageFormat catalogues extracted from the source and checked against it | question 4 of `11-surface-web.md` chose ICU for core and recipe strings alike |
| tests | Vitest and Testing Library for components; Playwright for the browser, against a real control plane, with its virtual WebAuthn authenticator for passkeys | a panel tested only against mocks tests the mocks; the browser tests run the same API the CLI's tests do |
| packages | pnpm, exact versions, a committed lockfile installed frozen, install scripts refused | the npm registry is the largest supply-chain surface the project has; no package runs code at install |

**The supply-chain policy of `deny.toml` holds for the bundle too.** Every package the bundle or
its build depends on must carry a licence from the same permissive list, and the check enforces it
from the lockfile; versions move only in a reviewed commit; and `scripts/check.sh` installs from the
lockfile, refuses an install script, type-checks, lints, and runs the component tests, so a commit
that breaks the panel fails like one that breaks the server.

> As built, 2026-10-17 (`web/`, `scripts/check-web.sh`). pnpm is pinned in `package.json` at an
> exact version and digest, which corepack verifies before running it; `.npmrc` refuses install
> scripts and saves exact versions, and the one package with a script, esbuild, is denied it by
> name in `pnpm-workspace.yaml`, since its script only checks a binary the build runs anyway. The
> check installs from the frozen lockfile, regenerates the API client from the schema into a
> scratch directory and fails on any difference from the committed `web/src/gen`, then
> type-checks, lints with typescript-eslint's strict type-checked rules, runs the component
> tests, builds, and reads every package's licence from the lockfile against the allow list of
> `deny.toml`, the one list for both. Two decisions were forced on the way. Vite is 7, not 8:
> Vite 8 bundles CSS through `lightningcss`, which is MPL-2.0, outside the list, and nothing in
> the panel needs what 8 adds. And the list gained four licences that ask less than MIT does,
> 0BSD, MIT-0, CC0-1.0, and BlueOak-1.0.0, which small packages in the build carry; one package
> is excepted by name with its reason in `web/licence-exceptions.json`, `caniuse-lite`, whose
> CC-BY-4.0 covers browser-support data the build reads and never ships. TypeScript is 6, the
> newest the lint rules support. What it costs: a newer Vite, or a dependency that moves to a
> licence outside the list, waits for a commit that argues it.

> As built, 2026-10-17 (`web/e2e`, `crates/korpis-e2e/tests/browser`). The browser tests are
> Playwright 1.63.0, pinned exactly like every package, driving the Chromium build it names,
> which `scripts/test-tools.sh` installs into `.tools/ms-playwright` with Playwright's own
> installer and checks against no digest pinned here, the one download in the test tools that is
> not verified, and so the trust placed in the Playwright release. They live in `web/e2e`, with
> their own TypeScript project and lint, and are run by a test of `korpis-e2e` that stands up
> the stack: a control plane serving the built panel on a free port, its passkeys bound to
> `localhost`; where a node can run here (root, beside containerd), a node with a workload on
> it; the invitations, the device code of a waiting `korpis login`, and the addresses, passed in
> the environment; and, after the browser is done, what only the machine can see, the file the
> browser uploaded compared with the node's copy and the CLI's sign-in confirmed. That test is a
> binary of its own, which cargo runs alone, because the node tests change the machine's network
> interfaces and Chromium abandons what is in flight when they do (Finding 88 of
> `23-walkthroughs.md`). Two fixtures stand in for time and trust: the harness moves a session's
> sign-in back eleven minutes when a test asks, rather than the ten-minute rule becoming
> configurable, and Chromium is told to ignore the control plane's certificate error, without
> which it registers no service worker (question 3 of §10 of `11-surface-web.md`).
>
> Added 2026-10-21: a person signing in through an identity provider is tested the same way,
> against the independent issuer of `conformance/oidc` (panva's oidc-provider) on a loopback
> port, serving HTTPS under a test authority that only the provider's client trusts, since the
> control plane sends a browser to no identity provider over plain HTTP (§6 of
> `16-extensions.md`), with `korpis-idp-oidc` installed into the organization and its workload on
> the node. The
> provider is served from the test's own process, through the identity socket and the relay the
> node serves that workload, as its `main` serves it inside the workload. It cannot run inside:
> the node's egress rules deny a workload every address of its own host (§7 of
> `07-networking.md`), and the issuer is on this one. What that leaves untested is the
> provider's `main` itself: reading its configuration from the environment, which `config`'s own
> tests cover, and building its HTTP client. Corrected 2026-10-22: `scripts/e2e-phase2.sh` runs
> that `main`, the provider's own binary, on the node's host in a mount namespace of its own that
> puts the workload's identity socket and relay where the workload's mount puts them, configured
> by its environment and trusting the test authority alone, and a person signs in through it,
> acts, and loses a group's authority by its refresh; so only the workload's own sandbox around
> it is left untested.
> `scripts/check.sh` runs them with the rest of the tests, and says so when no browser is
> installed and they are skipped. What it costs: about forty seconds of a check, and one
> download trusted on its publisher's word. Written over one day, they found eight defects that
> the component tests and the API tests had not (Findings 85 to 92 of `23-walkthroughs.md`).

The control plane's side of a passkey sign-in, the WebAuthn relying party (§9.6 of
`08-identity.md`), is Korpis's own: CBOR and COSE decoded by `ciborium` and `coset`, signatures
verified by `ring`, which the TLS stack already brings. It is tested against an independent
software authenticator, 1Password's `passkey` crate, which registers and asserts as a browser
would, in a package of its own (`conformance/webauthn`) with its own lockfile: the authenticator
turns on a `serde_json` feature that changes how JSON maps are ordered, and in the main workspace it
would have changed that for every crate's tests, so that they no longer tested what production
runs.

The OpenID Connect provider (§5.3 of `08-identity.md`) is held to the same standard. Its ID tokens
are verified by `ring` too, RS256, ES256, and EdDSA from the issuer's published keys, rather than
by a JWT library that would bring a second cryptographic stack and an algorithm list wider than
the three it accepts. It is tested against an independent issuer, panva's `oidc-provider`, which
is OpenID Certified, run as a host process from a package of its own (`conformance/oidc`) whose
lockfile, exact versions, refused install scripts, and licences are held to the panel's policy:
codes with PKCE, ID tokens in each algorithm, and refresh tokens it rotates and revokes on reuse,
which is how a test proved that two sets asking about one person at once would otherwise have
ended that person's grant (Finding 119 of `23-walkthroughs.md`). Only who signs in is the test's
own: the issuer's login and consent are answered by a script, not a form.

---

## 10. Open questions

1. **Agent supervisor language. Resolved: Rust, by question 6.** This was first resolved as Go, and
   the reasoning recorded with it still governs the part that matters: the tempting argument for a
   single-language agent is that it would remove a process boundary's worth of marshalling, and that
   argument inverts the design. **The boundary between the supervisor and the privileged components
   is not a cost to be optimized away; it is the point** (§6 of `17-security.md`). The supervisor is
   Rust because everything is, and it remains a separate, unprivileged process.
2. **Rust async runtime. Resolved here: Tokio, never in the privileged helper.** The filesystem
   worker serves SFTP (§6 of `02-architecture.md`), and the mature Rust SSH server implementations
   are built on Tokio, so the worker uses it. The order matters and is fixed: the helper starts the
   worker inside its prepared mount namespace (§6 of `02-architecture.md`), the worker applies its
   Landlock ruleset while it is still single-threaded, and only then starts the runtime, so every
   thread the runtime creates is born confined (§4). (Corrected 2026-10-22: the worker does not
   serve SFTP, and runs no runtime. A relay serves SFTP (§3.1 of `02-architecture.md`) as a client
   of the file session, and the worker answers that session's requests one at a time on its
   socket, synchronously, so the order above is now simply that it applies its ruleset before it
   serves anything. The relay's SSH server is the one built on Tokio, above.) The control plane,
   the relay, the supervisor,
   and the CLI use Tokio as well, now that they are Rust. The privileged helper stays synchronous
   and single-threaded: it serves one structured request at a time, and an async runtime would add
   threads and code to the one process where both are least welcome. It does not even link one:
   the generated Connect services, which bring Tokio with them, are a feature of the wire crate
   that the helper leaves off, and the repository's layer check fails if Tokio reaches it.
3. **Windows driver language. Carried by §9 of `04-runtimes.md`, question 6.** The default is now
   Rust, through Microsoft's `windows` crate, which reaches Job Objects and the Host Compute Service
   without a third toolchain. C# remains the fallback if an API the drivers need proves unreachable
   from it. Deferred to when Windows is actually built (§13 of `17-security.md` has the harder
   question).
4. **Web framework. Resolved in §10 of `11-surface-web.md`, question 1**: React, with the
   toolchain of §9 here.
5. **Intent body representation. Resolved in §3.1 of `03-state.md`**: proto3 canonical JSON in the
   `jsonb` column, so the `.proto` remains the only schema and the seam closes.
6. **Is the split still right, or should more of Korpis be Rust? Resolved in §3: Rust throughout**,
   decided with the maintainer on 2026-10-12. The question was reopened because the second external
   review showed that the share of Korpis sitting on a security boundary was far larger than the
   first version assumed. Once that was counted, the remaining arguments for Go (ecosystem,
   tooling, contributor pool) were checked one by one in §3, and only compile time survived.
7. **Depending on a pre-1.0 `connectrpc`.** The Rust Connect implementation passes the conformance
   suite but has not committed to a stable API. Korpis pins it, tracks its releases, and keeps its
   own API surface independent of the crate's types, so a breaking change is a contained update. The
   fallback, if the crate stalled, is to serve gRPC and gRPC-Web from `tonic` and keep the browser
   client on gRPC-Web, which loses plain-JSON `curl` access until a replacement exists. When to
   require a 1.0, and whether the fallback is ever exercised, is decided before Phase 0's protocol
   freeze. → here

   **Resolved here, 2026-10-13, as Phase 0 closes: the freeze does not wait for a 1.0.** What
   freezes is the wire contract, the schema in `crates/korpis-proto` and the Connect, gRPC, and
   gRPC-Web protocols as their own specifications define them; the conformance suite tests the
   crate against those, and neither is a property of the crate's Rust API. Every Korpis crate pins
   `connectrpc` and `buffa` to an exact version, so no release reaches a build unreviewed, and
   the domain and application crates never hold a generated type, which the layer rule checks
   (§3 of `ARCHITECTURE.md`). A breaking release of the crate is therefore an update to the adapters
   that changes no byte on the wire, which the API, agent, and CLI tests check end to end. The
   fallback stays as described and is exercised only if a release Korpis needs fails the
   conformance suite, or a security advisory against the crate goes unfixed upstream. What this
   costs: until a 1.0, every upgrade of the crate is reviewed by hand rather than taken as a
   compatible bump, and a long stall would leave Korpis maintaining a pinned version it did not
   write.
