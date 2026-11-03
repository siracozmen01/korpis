**English** · [Türkçe](./README.tr.md)

# Korpis

**A workload platform for machines you control.**

Korpis runs anything with a lifecycle (a game server, a bot, a web service, a database, a scheduled
job, a virtual machine) on hardware you own. It places the workload on a node, isolates it as
strongly as you ask, gives it storage and network, keeps it in the state you declared, and lets you
and the people you delegate to operate it from a panel, a terminal, chat, or the API.

> **Not released yet.** Phases 0 to 3 of the build are done in `crates/` (Rust) and `web/`
> (TypeScript): one operator on one machine installs it, runs workloads, backs them up and restores
> them, and reads an honest console from the panel or the CLI, with every limit it shows enforced by
> the kernel; it hosts other people: organizations delegate to organizations beneath them with
> quota that inherits, people sign in through their own identity provider with no account made for
> them, and what each tenant uses is metered; and it runs many nodes: a workload moves between them
> with its volumes, its public address and its meter, and runs in a microVM where a container is
> not isolation enough. [`scripts/e2e-phase0.sh`](./scripts/e2e-phase0.sh) to
> [`e2e-phase3.sh`](./scripts/e2e-phase3.sh) check it as an operator would; the
> [milestones](https://github.com/siracozmen01/korpis/milestones) say what comes next.

---

## What it does differently

**Declares, never commands.** You say what should be true. Korpis converges reality toward it,
continuously, and reports what is actually true. No component tells another to do something and
assumes it worked, which is where "the panel says stopped but the server is running" comes from.

**Makes the diff a real object.** Every change produces an inspectable plan before it produces an
effect. Dry runs, approvals, scheduled changes, rollback, and an audit trail that cannot lie are
consequences of that one decision rather than five separate features.

**Has no roles.** Authority is a *grant* (a subject, some actions, a scope, some conditions) and a
grant can only ever produce weaker children. So "this link lets your friend restart this one server
for the next 24 hours, no account needed" is an ordinary operation, and reselling is something the
model does for free rather than a feature anyone writes.

**Lets you choose the isolation.** `process`, `container`, `microvm`, `vm`, per workload, behind one
driver interface. Each tier is documented by what it does *not* stop as much as by what it does,
because a tier that looks like isolation and isn't is worse than one honestly labelled weaker.

**Never claims what it doesn't enforce.** Every displayed limit is enforced by the kernel and
metered. Every status is observed. When Korpis can't see something it says `unknown` instead of
guessing, and when it drops data it leaves a marked gap instead of a clean-looking hole.

**Doesn't tax growth.** No per-workload, per-node, per-instance, per-account, or per-domain charge,
at any scale, ever. No editions, no gated features, no commercial-use restriction.

---

## Start here

| You want to | Read |
|---|---|
| understand what it is and how the parts fit | [overview](./docs/internals/overview.md), then the [glossary](./docs/internals/glossary.md) |
| install it and add nodes | [install](./docs/user/install.md), [nodes](./docs/user/nodes.md), [recovery](./docs/user/recovery.md) |
| know whether it is safe | [security](./docs/internals/security.md), [grants](./docs/internals/grants.md), [runtimes](./docs/internals/runtimes.md) |
| build on it | [api](./docs/internals/api.md), [extensions](./docs/internals/extensions.md), [recipes](./docs/internals/recipes.md) |
| work on the code | [CONTRIBUTING.md](./CONTRIBUTING.md), [development](./docs/operations/development.md), [architecture](./docs/internals/architecture.md) |
| know who decides | [GOVERNANCE.md](./GOVERNANCE.md) |

```
docs/internals/    the design the code is held to: one living page per topic
docs/user/         installing and operating it, by task
docs/operations/   developing, checking and releasing it
crates/            the implementation, a Rust workspace
web/               the panel, a static TypeScript bundle
AGENTS.md          the rules anyone changing the repository works under
```

Undecided design questions are [issues labelled `question`](https://github.com/siracozmen01/korpis/issues?q=is%3Aissue+label%3Aquestion).
A deferred decision with a name on it is honest; an undeclared one is a trap someone walks into later.

---

## How this is written

**By one person, with Claude.** That is worth saying plainly rather than leaving for someone to
notice. Nobody should take a design on trust because of who or what produced it, so:

- **Every departure from common practice is argued from something citable, or says it is a
  decision.** Claims about other projects link their issue, advisory or documentation inline.
- **The design was attacked on purpose.** Concrete scenarios were traced across every layer they
  touch, looking for the step where nothing holds the ball, and independent reviews read every
  definition asking what evaluates it, where, and with what information. Each defect found was
  fixed in the design and the code, and is in the git history.
- **Every limit is tested where it is enforced.** Tests run against real PostgreSQL, a real registry,
  real containerd and real microVMs, and the panel is driven in a real browser against a real
  control plane.

The judgement calls, the priorities, and the mistakes are the author's. If something here is wrong,
open an issue and say so.

---

## Contributing

The most useful thing anyone can do is **argue with it**: a reasoning gap, an unstated assumption, a
failure mode nobody traced, a claim about another project that is wrong or unfair, or experience
running this class of software at scale. See [CONTRIBUTING.md](./CONTRIBUTING.md). To report a
vulnerability, see [SECURITY.md](./SECURITY.md).

---

## License

| Layer | License |
|---|---|
| Protocol definitions, client SDKs, recipe format, agent protocol, conformance suites | **Apache-2.0** |
| Control plane, node agent, relay, panel, CLI and chat clients | **AGPL-3.0-or-later** |
| Documentation | **CC BY 4.0** |
| Recipes, extensions, themes | Author's choice, proprietary included |

Plainly, because AGPL gets misread as a restriction on use: **run it commercially, sell capacity on
it, charge what you like, build closed extensions against it.** Running it unmodified for your
customers obliges nothing. Modifying it and offering the modified version over a network means
offering its users your source; distributing copies means the source goes with them, which an
unmodified build already arranges.

Contributions come in under a Developer Certificate of Origin, with no CLA and no copyright
assignment, so nobody can relicense a contributor's code without them. The licence does not by
itself rule out an open-core pivot; the project's policy does, and [GOVERNANCE.md](./GOVERNANCE.md)
says which is which.
