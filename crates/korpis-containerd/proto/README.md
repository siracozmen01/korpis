# containerd's API, vendored

These `.proto` files are containerd's own, copied unchanged from the `api/` module of
containerd **v2.3.6** (`https://github.com/containerd/containerd/archive/refs/tags/v2.3.6.tar.gz`,
sha256 `b4b407c64e11a22549b14c47857ac0e02e9f47f8cfc28924e9aa04a0adcaa883`). They are licensed
under the Apache License 2.0 by the containerd Authors; the licence is in `LICENSE` beside this
file, and each file keeps its own header.

Only the services the `oci` driver uses, and the types they import, are here: containers,
content, images, leases, namespaces, snapshots, tasks, transfer, and version. To update: replace
the files from a newer tag of the same API module, record the tag and digest above, and run
`scripts/gen-proto.sh`. containerd keeps this API backward compatible within its major version.
