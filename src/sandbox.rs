//! Fail-closed Linux filesystem boundary. No whole-host bind and no daemon credentials.
use crate::{codex::absolute_executable, config::Project};
use anyhow::{Result, ensure};
use std::path::Path;
use tokio::process::Command;
pub fn command(p: &Project, worktree: &Path, writable: bool, network: bool) -> Result<Command> {
    ensure!(
        worktree.is_absolute() && worktree.is_dir(),
        "sandbox worktree missing"
    );
    let mut c = Command::new("bwrap");
    c.env_clear().args([
        "--die-with-parent",
        "--new-session",
        "--unshare-pid",
        "--unshare-ipc",
        "--unshare-uts",
    ]);
    if !network {
        c.arg("--unshare-net");
    }
    for dir in ["/usr", "/bin", "/sbin", "/lib", "/lib64"] {
        if Path::new(dir).exists() {
            c.arg("--ro-bind").arg(dir).arg(dir);
        }
    }
    for f in [
        "/etc/ssl",
        "/etc/resolv.conf",
        "/etc/hosts",
        "/etc/nsswitch.conf",
        "/etc/passwd",
        "/etc/group",
    ] {
        if Path::new(f).exists() {
            c.arg("--ro-bind").arg(f).arg(f);
        }
    }
    c.args([
        "--proc",
        "/proc",
        "--dev",
        "/dev",
        "--tmpfs",
        "/tmp",
        "--dir",
        "/home/agent",
        "--setenv",
        "HOME",
        "/home/agent",
        "--setenv",
        "PATH",
        "/usr/local/bin:/usr/bin:/bin",
        "--setenv",
        "LANG",
        "C.UTF-8",
        "--setenv",
        "GIT_TERMINAL_PROMPT",
        "0",
    ]);
    for path in &p.toolchain_paths {
        let path = path.canonicalize()?;
        let home = std::env::var_os("HOME")
            .map(std::path::PathBuf::from)
            .unwrap_or_default();
        ensure!(
            !path.starts_with(&p.repo)
                && !home.starts_with(&path)
                && !path.components().any(|c| matches!(
                    c.as_os_str().to_str(),
                    Some(".codex" | ".ssh" | ".aws" | ".gnupg" | ".config")
                ))
                && !path.ends_with(".cargo"),
            "unsafe toolchain path; mount only specific toolchains or registry caches"
        );
        c.arg("--ro-bind").arg(&path).arg(&path);
    }
    let extra = p
        .toolchain_paths
        .iter()
        .filter(|p| p.join("bin").is_dir())
        .map(|p| p.join("bin").to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    if !extra.is_empty() {
        c.args([
            "--setenv",
            "PATH",
            &format!("{}:/usr/local/bin:/usr/bin:/bin", extra.join(":")),
        ]);
    }
    for path in &p.toolchain_paths {
        if path.ends_with("registry") {
            c.args(["--dir", "/home/agent/.cargo", "--ro-bind"])
                .arg(path)
                .arg("/home/agent/.cargo/registry")
                .args(["--setenv", "CARGO_HOME", "/home/agent/.cargo"]);
        }
    }
    c.arg(if writable { "--bind" } else { "--ro-bind" })
        .arg(worktree)
        .arg("/work");
    if worktree.join(".git").exists() {
        c.args(["--ro-bind", "/dev/null", "/work/.git"]);
    }
    // Project Codex configuration must not expand capabilities.
    if worktree.join(".codex").is_dir() {
        c.args(["--tmpfs", "/work/.codex"]);
    }
    c.args(["--chdir", "/work"]);
    Ok(c)
}
pub fn mount_executable(c: &mut Command, path: &Path) -> Result<()> {
    let executable = absolute_executable(path)?;
    let parent = executable
        .parent()
        .ok_or_else(|| anyhow::anyhow!("Codex executable parent missing"))?;
    if parent.join("codex-code-mode-host").is_file() {
        c.arg("--ro-bind").arg(parent).arg("/opt");
    } else {
        c.arg("--ro-bind").arg(executable).arg("/opt/codex");
    }
    Ok(())
}
