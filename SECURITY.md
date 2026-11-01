# Security policy

## Reporting a vulnerability

Report it privately through this repository's **Security** tab, **Report a vulnerability** (GitHub's
private vulnerability reporting). Do not open a public issue for something exploitable in the code.
The response window is stated here before the first release.

No release has shipped, so nothing is deployed under a supported version, but the code is real: the
control plane, the node agent and its privileged helper, the relay, the panel and the CLI. A flaw in
any of them is worth reporting now.

A weakness in the design rather than the code (a boundary drawn in the wrong place, a protocol that
leaks, an authorization rule that can be widened, a failure with no containment) may be discussed in a
public issue when it is not exploitable as built. Especially valuable:

- anything in [security](docs/internals/security.md) that overstates a guarantee: the page tries to
  say what each isolation tier does not stop, and optimism anywhere is a defect;
- a way for a grant to gain authority its parent did not have ([grants](docs/internals/grants.md));
- a path where tenant-controlled input reaches a privileged context;
- a resource a tenant can exhaust that is not bounded.

## Advisories

A fix is published with an advisory giving what was reachable, what it affected, which versions, and
what the fix changed. A fix made under embargo is reviewed in public the day it is disclosed. A project
that learned as much as this one did from other people's advisories does not publish vague ones.

## Scope

This policy covers this repository. A finding about a third-party system cited in the documentation
belongs to that project.
