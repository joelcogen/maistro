use maistro::process;
use std::{fs, time::Duration};
use tokio::process::Command;
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn cancellation_escalates_and_reaps_a_worker_ignoring_sigterm() {
    let dir = tempfile::tempdir().unwrap();
    let pid_path = dir.path().join("pid");
    let mut cmd = Command::new("python3");
    cmd.args(["-c", "import os, signal, sys, time\nsignal.signal(signal.SIGTERM, signal.SIG_IGN)\nopen(sys.argv[1], 'w').write(str(os.getpid()))\ntime.sleep(60)"]).arg(&pid_path);
    let token = CancellationToken::new();
    let cancel = token.clone();
    let task =
        tokio::spawn(
            async move { process::run(cmd, "", Duration::from_secs(10), &cancel, None).await },
        );
    let started = tokio::time::timeout(Duration::from_secs(3), async {
        while !pid_path.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    token.cancel();
    let result = tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .unwrap()
        .unwrap();
    assert!(started.is_ok());
    let pid: u32 = fs::read_to_string(pid_path).unwrap().parse().unwrap();
    assert!(result.err().unwrap().to_string().contains("cancelled"));
    assert!(!process::group_exists(pid));
}

#[tokio::test]
async fn cancellation_stops_worker_and_its_child_command() {
    let dir = tempfile::tempdir().unwrap();
    let pid_path = dir.path().join("pid");
    let mut cmd = Command::new("python3");
    cmd.args(["-c", "import os, signal, subprocess, sys, time\nchild=subprocess.Popen(['sleep', '60'])\ndef stop(*args):\n child.wait(timeout=2)\n sys.exit(0)\nsignal.signal(signal.SIGTERM, stop)\nopen(sys.argv[1], 'w').write(str(os.getpid())+' '+str(child.pid))\ntime.sleep(60)"]).arg(&pid_path);
    let token = CancellationToken::new();
    let cancel = token.clone();
    let task =
        tokio::spawn(
            async move { process::run(cmd, "", Duration::from_secs(10), &cancel, None).await },
        );
    let started = tokio::time::timeout(Duration::from_secs(3), async {
        while !pid_path.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    token.cancel();
    let result = tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .unwrap()
        .unwrap();
    assert!(started.is_ok());
    assert!(result.err().unwrap().to_string().contains("cancelled"));
    let pids = fs::read_to_string(pid_path).unwrap();
    let pids: Vec<i32> = pids
        .split_whitespace()
        .map(|p| p.parse().unwrap())
        .collect();
    assert!(!process::group_exists(pids[0] as u32));
    assert_eq!(unsafe { libc::kill(pids[1], 0) }, -1);
}
