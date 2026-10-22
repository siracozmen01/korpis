# The microVM guest kernel's configuration

The kernel every `microvm` workload boots is the node's, built by `scripts/guest-kernel.sh` from
a kernel.org longterm release pinned by its SHA-256, with the configuration in this directory
(§6 of `docs/04-runtimes.md`). A recipe does not bring a kernel.

Two files are Firecracker's guest configurations, vendored unchanged from the
[Firecracker repository](https://github.com/firecracker-microvm/firecracker) at tag `v1.17.0`,
under its Apache-2.0 licence, as its own continuous integration builds its guest kernels: the
first is the base, the second adds what its tests need (among them the keyboard controller a
guest's Ctrl+Alt+Del arrives through, which is how a clean stop reaches a guest).

| File | From `resources/guest_configs/` | SHA-256 |
|---|---|---|
| `microvm-kernel-ci-x86_64-6.18.config` | the same name | `ba22401a0c7292a4c024ebcd10a562d4a1f1bfd2faed671406d3b159c0cf5215` |
| `ci.config` | the same name | `6ce47107d9fb695a873ffc4733a1d8e1b4941c4f272e20dac4bf163dfb77f47e` |

The script concatenates them in that order and lets the kernel's `make olddefconfig` resolve what
the release adds, as Firecracker's `resources/rebuild.sh` does. Firecracker builds the same
configuration against Amazon Linux's kernel tree; Korpis builds it against kernel.org's, so a
node's guest kernel is one anyone can rebuild from public sources, and the script stamps the build
with fixed values, so the same compiler gives the same digest: two builds with GCC 13.3.0 on
Ubuntu 24.04, in different directories, both gave `vmlinux` the SHA-256
`22907bf21c0fe09af893e0a5bd4637dcab79a8882f8f30a32633fbe0fe15e4f3` (2026-10-23).

Moving to a newer Firecracker or kernel release is a reviewed commit that replaces these files,
updates the table, and changes the pinned version and digest in the script.
