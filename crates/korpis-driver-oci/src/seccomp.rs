// SPDX-License-Identifier: AGPL-3.0-or-later

//! The container tier's seccomp profile (§2 of docs/04-runtimes.md). An allowlist: a system call
//! not named here fails with `EPERM`, so a call added to the kernel tomorrow is refused until it
//! is reviewed. It starts from the profile containerd and Docker ship by default and is narrower:
//! no `io_uring`, whose interface has been a steady source of kernel escalations; no `ptrace`,
//! `process_vm_readv` or `process_vm_writev`; no namespace creation or joining (`unshare`,
//! `setns`, and `clone` with namespace flags) and no mounting, which inside a user namespace
//! would otherwise be open to the workload; no keyrings; and no `name_to_handle_at`. `clone3` is
//! answered with `ENOSYS`, because its flags are behind a pointer seccomp cannot read, and libc
//! falls back to `clone`, whose flags it can.

use serde::Serialize;

/// Allowed unconditionally, one name per line of `seccomp/allowed.txt`.
const ALLOWED: &str = include_str!("seccomp/allowed.txt");

/// The names [`ALLOWED`] lists.
fn allowed() -> Vec<&'static str> {
    ALLOWED.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')).collect()
}

/// The namespace flags of `clone`: `CLONE_NEWNS`, `NEWCGROUP`, `NEWUTS`, `NEWIPC`, `NEWUSER`,
/// `NEWPID`, `NEWNET`, and `NEWTIME`.
const NAMESPACE_FLAGS: u64 = 0x7E02_0080;
/// `AF_VSOCK`: a path out of a virtual machine's guest, never a container's business.
const AF_VSOCK: u64 = 40;
/// The `personality` values programs legitimately ask for: `PER_LINUX`, `PER_LINUX32`,
/// `UNAME26` alone and with `PER_LINUX32`, and the query that changes nothing.
const PERSONALITIES: [u64; 5] = [0, 8, 0x0002_0000, 0x0002_0008, 0xffff_ffff];
const EPERM: u32 = 1;
const ENOSYS: u32 = 38;

/// A condition on an argument.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Arg {
    index: u32,
    value: u64,
    value_two: u64,
    op: &'static str,
}

/// One rule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Rule {
    names: Vec<&'static str>,
    action: &'static str,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    args: Vec<Arg>,
    #[serde(skip_serializing_if = "Option::is_none")]
    errno_ret: Option<u32>,
}

/// The profile, as the OCI runtime specification carries it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Seccomp {
    default_action: &'static str,
    default_errno_ret: u32,
    architectures: Vec<&'static str>,
    syscalls: Vec<Rule>,
}

fn allow(names: Vec<&'static str>, args: Vec<Arg>) -> Rule {
    Rule { names, action: "SCMP_ACT_ALLOW", args, errno_ret: None }
}

fn arg(index: u32, value: u64, value_two: u64, op: &'static str) -> Arg {
    Arg { index, value, value_two, op }
}

/// The architectures of this machine's kernel's syscall tables a process here may use.
fn architectures() -> Vec<&'static str> {
    match std::env::consts::ARCH {
        "aarch64" => vec!["SCMP_ARCH_AARCH64", "SCMP_ARCH_ARM"],
        _ => vec!["SCMP_ARCH_X86_64", "SCMP_ARCH_X86", "SCMP_ARCH_X32"],
    }
}

/// The profile.
#[must_use]
pub fn profile() -> Seccomp {
    let mut syscalls = vec![allow(allowed(), Vec::new())];
    syscalls.push(allow(vec!["clone"], vec![arg(0, NAMESPACE_FLAGS, 0, "SCMP_CMP_MASKED_EQ")]));
    syscalls.push(Rule {
        names: vec!["clone3"],
        action: "SCMP_ACT_ERRNO",
        args: Vec::new(),
        errno_ret: Some(ENOSYS),
    });
    syscalls.push(allow(vec!["socket"], vec![arg(0, AF_VSOCK, 0, "SCMP_CMP_NE")]));
    for p in PERSONALITIES {
        syscalls.push(allow(vec!["personality"], vec![arg(0, p, 0, "SCMP_CMP_EQ")]));
    }
    Seccomp {
        default_action: "SCMP_ACT_ERRNO",
        default_errno_ret: EPERM,
        architectures: architectures(),
        syscalls,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_profile_denies_by_default_and_never_allows_an_escape_hatch() {
        let p = profile();
        assert_eq!(p.default_action, "SCMP_ACT_ERRNO");
        let allowed: Vec<&str> = p
            .syscalls
            .iter()
            .filter(|r| r.action == "SCMP_ACT_ALLOW" && r.args.is_empty())
            .flat_map(|r| r.names.iter().copied())
            .collect();
        for never in [
            "io_uring_setup",
            "ptrace",
            "unshare",
            "setns",
            "mount",
            "umount2",
            "pivot_root",
            "keyctl",
            "add_key",
            "bpf",
            "perf_event_open",
            "init_module",
            "kexec_load",
            "open_by_handle_at",
            "userfaultfd",
            "process_vm_readv",
            "clone",
            "clone3",
            "chroot",
        ] {
            assert!(!allowed.contains(&never), "{never} must not be allowed unconditionally");
        }
        let json = serde_json::to_value(&p).unwrap();
        assert_eq!(json["defaultErrnoRet"], 1);
        assert!(json["syscalls"].as_array().unwrap().iter().any(|r| r["errnoRet"] == 38));
    }
}
