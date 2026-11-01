# Release

How Korpis is versioned, released, supported and deprecated. No release has shipped yet; this is
what the first one is held to. Who decides is in [GOVERNANCE.md](../../GOVERNANCE.md).

## Contents

- [Versioning](#versioning)
- [Cadence](#cadence)
- [Long-term support](#long-term-support)
- [Deprecation](#deprecation)
- [Artifacts](#artifacts)
- [Before the first release](#before-the-first-release)

## Versioning

Semantic versioning, with the public surface named, because "SemVer" without that is an argument
waiting to happen:

| Public: a breaking change needs a major version | Not public |
|---|---|
| the agent protocol | the database schema |
| the client API schema (`korpis.v1`) | the internal crate layout |
| the recipe format and its install language | the panel's appearance and layout |
| the extension contract | log message wording |
| the CLI's `--json` output and exit codes | the CLI's human-readable output |
| storage class and driver capability names | performance characteristics |

**Actions are never renamed or split**, at any version, a major one included: a grant issued under
`v1` is a delegation someone made deliberately, and a major version is not permission to reinterpret
it. Before the first release the wire contracts change on design merit alone; after it they change
compatibly.

## Cadence

Time-based, not feature-based: a minor release on a fixed cadence, targeting every eight weeks,
containing whatever is ready. A feature-based date is how a rewrite gets announced and abandoned; a
train that leaves on schedule cannot be held by one unfinished feature.

## Long-term support

One release a year is long-term support, maintained for twenty-four months with security and
correctness fixes. A host with sixty nodes and paying customers cannot requalify a platform six
times a year, and the incumbents charge for exactly this stability; here it is free.

The commitment constrains the protocol: a long-term control plane speaks to agents for its whole
supported life, so the agent protocol's supported window is a dated obligation, and a proposed
protocol change shows its cost in review rather than afterwards. The control plane supports the
current and two preceding agent protocol versions and upgrades first ([node
agent](../internals/node-agent.md)).

## Deprecation

Announce in a release note, then warn in band in API responses and CLI output, then remove no sooner
than the next long-term release. Nothing is removed without first warning in the channel where it is
used; a deprecation only in release notes is one nobody saw.

## Artifacts

A release is cut only from a commit whose `full` check passed ([checks](checks.md)). Builds are
reproducible from the repository, and every artifact is signed and recorded in a public transparency
log, so anyone can check a release is what its tag produces. Release keys are subkeys of an offline
root with an escrowed copy. Every binary and the panel name the exact source revision they were
built from and where it is published (`korpis version`, `/about/source`), which meets the AGPL for
conveyed copies as shipped.

## Before the first release

The first release waits on the succession document ([GOVERNANCE.md](../../GOVERNANCE.md)), the
signing and transparency pipeline above, a private disclosure path with a stated response window
([SECURITY.md](../../SECURITY.md)), and the milestones that precede it. Each is an issue in the
First release milestone.
