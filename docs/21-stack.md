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
  waved away.
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
| `oci` | containerd, over its gRPC API, through containerd's own Rust client crate | the driver's spec validation is security-critical (§6.1 of `02-architecture.md`) |
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
framework remains open (§10 of `11-surface-web.md`) and the constraints that will decide it are
already fixed: static output, streams rather than polling, correct rendering for a subject holding
an arbitrary grant set, and small enough that a host serving it from a cheap CDN is ordinary.

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
   thread the runtime creates is born confined (§4). The control plane, the relay, the supervisor,
   and the CLI use Tokio as well, now that they are Rust. The privileged helper stays synchronous
   and single-threaded: it serves one structured request at a time, and an async runtime would add
   threads and code to the one process where both are least welcome.
3. **Windows driver language. Carried by §9 of `04-runtimes.md`, question 6.** The default is now
   Rust, through Microsoft's `windows` crate, which reaches Job Objects and the Host Compute Service
   without a third toolchain. C# remains the fallback if an API the drivers need proves unreachable
   from it. Deferred to when Windows is actually built (§13 of `17-security.md` has the harder
   question).
4. **Web framework. Carried by §10 of `11-surface-web.md`, question 1**, where the constraints that
   will decide it are listed.
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
