//! Bounded, noninteractive process groups. Dropping a future kills its entire group.
use anyhow::{Context, Result, ensure};
use std::{path::Path, process::Stdio, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::Command,
};
use tokio_util::sync::CancellationToken;
const MAX_OUTPUT: usize = 16 * 1024 * 1024;
pub struct Output {
    pub stdout: String,
    pub stderr: String,
    pub code: i32,
    pub pid: u32,
}
struct Group {
    pid: u32,
    armed: bool,
}
impl Drop for Group {
    fn drop(&mut self) {
        if self.armed {
            unsafe {
                libc::kill(-(self.pid as i32), libc::SIGKILL);
            }
        }
    }
}
#[derive(Debug)]
pub struct UnconfirmedTermination {
    pub pid: u32,
}
impl std::fmt::Display for UnconfirmedTermination {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "process group {} termination is unconfirmed; automatic execution held",
            self.pid
        )
    }
}
impl std::error::Error for UnconfirmedTermination {}
pub fn group_exists(pid: u32) -> bool {
    let result = unsafe { libc::kill(-(pid as i32), 0) };
    result == 0 || std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
}
async fn terminate(child: &mut tokio::process::Child, pid: u32) -> Result<()> {
    if group_exists(pid) {
        unsafe {
            libc::kill(-(pid as i32), libc::SIGTERM);
        }
    }
    if tokio::time::timeout(Duration::from_millis(300), child.wait())
        .await
        .is_err()
    {
        unsafe {
            libc::kill(-(pid as i32), libc::SIGKILL);
        }
    }
    let _ = tokio::time::timeout(Duration::from_secs(3), child.wait()).await;
    if group_exists(pid) {
        unsafe {
            libc::kill(-(pid as i32), libc::SIGKILL);
        }
    }
    for _ in 0..100 {
        if !group_exists(pid) {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    Err(UnconfirmedTermination { pid }.into())
}
async fn read_bounded(
    mut r: impl tokio::io::AsyncRead + Unpin,
    log: Option<std::path::PathBuf>,
) -> Result<Vec<u8>> {
    use std::io::Write;
    let mut file = log.map(std::fs::File::create).transpose()?;
    let mut bytes = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        let n = r.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        ensure!(
            bytes.len() + n <= MAX_OUTPUT,
            "subprocess output exceeds 16 MiB"
        );
        if let Some(f) = &mut file {
            f.write_all(&buf[..n])?;
        }
        bytes.extend_from_slice(&buf[..n]);
    }
    Ok(bytes)
}
pub async fn run(
    mut cmd: Command,
    input: &str,
    timeout: Duration,
    cancel: &CancellationToken,
    log: Option<&Path>,
) -> Result<Output> {
    ensure!(!cancel.is_cancelled(), "subprocess cancelled before spawn");
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .process_group(0);
    // Linux parent-death signal closes the crash-before-PID-recording window.
    let parent_pid = std::process::id() as i32;
    unsafe {
        cmd.pre_exec(move || {
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::getppid() != parent_pid {
                libc::kill(libc::getpid(), libc::SIGKILL);
            }
            Ok(())
        });
    }
    let mut child = cmd.spawn().context("spawn subprocess")?;
    let pid = child.id().context("missing child pid")?;
    let mut group = Group { pid, armed: true };
    let mut stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    if let Some(p) = log {
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let terminated = p.with_extension("terminated");
        if terminated.exists() {
            std::fs::remove_file(terminated)?;
        }
        std::fs::write(
            p.with_extension("pid"),
            serde_json::to_vec(
                &serde_json::json!({"pid":pid,"start":process_start(pid),"boot":boot_id()}),
            )?,
        )?;
    }
    let operation = async {
        let write = async {
            stdin.write_all(input.as_bytes()).await?;
            drop(stdin);
            Ok::<_, anyhow::Error>(())
        };
        let (_, out, err, status) = tokio::try_join!(
            write,
            read_bounded(stdout, log.map(Path::to_path_buf)),
            read_bounded(stderr, log.map(|p| p.with_extension("stderr"))),
            async { Ok::<_, anyhow::Error>(child.wait().await?) }
        )?;
        Ok::<_, anyhow::Error>(Output {
            stdout: String::from_utf8(out).context("stdout not UTF-8")?,
            stderr: String::from_utf8_lossy(&err).into(),
            code: status.code().unwrap_or(-1),
            pid,
        })
    };
    let result = tokio::select! {r=tokio::time::timeout(timeout,operation)=>match r{Ok(r)=>r,Err(_)=>Err(anyhow::anyhow!("subprocess timed out"))},_=cancel.cancelled()=>Err(anyhow::anyhow!("subprocess cancelled"))};
    terminate(&mut child, pid).await?;
    group.armed = false;
    if let Some(p) = log {
        std::fs::write(
            p.with_extension("terminated"),
            "process group confirmed gone\n",
        )?;
    }
    if let (Some(p), Ok(o)) = (log, &result) {
        std::fs::write(p, &o.stdout)?;
        std::fs::write(p.with_extension("stderr"), &o.stderr)?;
    }
    result
}
pub async fn checked(cmd: Command, timeout: Duration) -> Result<String> {
    let o = run(cmd, "", timeout, &CancellationToken::new(), None).await?;
    ensure!(
        o.code == 0,
        "command failed (exit {}): {}",
        o.code,
        o.stderr.chars().take(2000).collect::<String>()
    );
    Ok(o.stdout.trim().into())
}

fn boot_id() -> String {
    std::fs::read_to_string("/proc/sys/kernel/random/boot_id")
        .unwrap_or_default()
        .trim()
        .into()
}
fn process_start(pid: u32) -> Option<u64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    stat.rsplit_once(") ")?
        .1
        .split_whitespace()
        .nth(19)?
        .parse()
        .ok()
}
/// Only unfinished process records are checked; PID identity prevents killing/reusing unrelated PIDs.
pub fn reconcile_groups(root: &Path) -> Result<()> {
    if !root.exists() {
        return Ok(());
    }
    for entry in std::fs::read_dir(root)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_symlink() {
            continue;
        }
        let path = entry.path();
        if kind.is_dir() {
            reconcile_groups(&path)?;
        } else if path.extension().is_some_and(|x| x == "pid")
            && !path.with_extension("terminated").exists()
        {
            let value: serde_json::Value = serde_json::from_slice(&std::fs::read(&path)?)?;
            let pid = value["pid"]
                .as_u64()
                .and_then(|p| u32::try_from(p).ok())
                .context("invalid process identity record")?;
            if value["boot"].as_str() != Some(&boot_id()) {
                continue;
            }
            if let (Some(recorded), Some(current)) = (value["start"].as_u64(), process_start(pid))
                && recorded != current
            {
                continue;
            }
            if group_exists(pid) {
                return Err(UnconfirmedTermination { pid }.into());
            }
            std::fs::write(
                path.with_extension("terminated"),
                "recovery confirmed process group gone\n",
            )?;
        }
    }
    Ok(())
}
