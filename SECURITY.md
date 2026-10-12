# Security Policy

## Current status

**There is no software here yet.** This repository contains a design specification, so there is
nothing deployed to exploit. This policy exists now anyway, because §1 of
[`19-governance.md`](./docs/19-governance.md) treats governance as a day-one decision and a project
without a disclosure path receives its first vulnerability as a public post.

## Reporting a vulnerability in the design

A flaw in a specification is worth reporting before it is a flaw in software, and it is far cheaper
to fix.

If you believe a design decision here creates a security weakness (a boundary drawn in the wrong
place, a protocol that leaks, an authorization rule that can be widened, a failure mode with no
containment), **open a public issue**. There is nothing to exploit yet, so there is no reason to
withhold the discussion, and a public argument produces a better fix.

Especially valuable:

- Anything in [`17-security.md`](./docs/17-security.md) that overstates a guarantee. That document
  tries hard to say what each isolation tier does *not* stop; if it is optimistic anywhere, that is
  a defect.
- A way for a grant to gain authority its parent did not have
  ([`08-identity.md`](./docs/08-identity.md) §3.2).
- A path where tenant-controlled input reaches a privileged context
  ([`17-security.md`](./docs/17-security.md) §3).
- A resource a tenant can exhaust that §10 of [`17-security.md`](./docs/17-security.md) does not
  index.

## Once software exists

When the first release ships, this section will carry a private disclosure address and a stated
response window, and coordinated disclosure will apply from that point. Advisories will be
published with the same detail this project's own research demanded of others: what was reachable,
what it affected, which versions, and what the fix actually changed.

A project that learned as much as this one did from reading other people's advisories does not get
to publish vague ones.

## Scope

This policy covers the contents of this repository. Findings about the third-party systems
discussed in [`docs/research/evidence.md`](./docs/research/evidence.md) should go to those
projects, not here. Every security advisory recorded in that document was responsibly disclosed to
and fixed by its maintainers. The ordinary issues it cites are a mix: some fixed, some declined,
some still open, and the document says which for each. All of them are listed only because the
*pattern* across them informed a design decision.

> Corrected 2026-10-12. This said every issue in the evidence document had been disclosed and
> fixed, which was true of the advisories and not of the issues.
