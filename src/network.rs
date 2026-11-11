//! Model-only egress for externally sandboxed Codex. Workers have no host network.
use anyhow::{Context, Result, ensure};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream, UnixListener, UnixStream},
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;
pub fn allowed(host: &str) -> bool {
    matches!(
        host,
        "chatgpt.com:443" | "api.openai.com:443" | "auth.openai.com:443" | "ab.chatgpt.com:443"
    )
}
pub struct Proxy {
    pub socket: PathBuf,
    token: CancellationToken,
    task: JoinHandle<()>,
}
impl Proxy {
    pub fn start(_dir: &Path) -> Result<Self> {
        use std::os::unix::fs::DirBuilderExt;
        let dir =
            std::env::temp_dir().join(format!("maistro-net-{}", uuid::Uuid::new_v4().simple()));
        std::fs::DirBuilder::new().mode(0o700).create(&dir)?;
        let socket = dir.join("model.sock");
        let listener = UnixListener::bind(&socket)?;
        let token = CancellationToken::new();
        let cancel = token.clone();
        let task = tokio::spawn(async move {
            let permits = std::sync::Arc::new(tokio::sync::Semaphore::new(32));
            let mut connections = tokio::task::JoinSet::new();
            loop {
                tokio::select! {_=cancel.cancelled()=>break,accepted=listener.accept()=>{let Ok((mut stream,_))=accepted else{break;};let Ok(permit)=permits.clone().try_acquire_owned()else{continue;};connections.spawn(async move{let _permit=permit;let _=tokio::time::timeout(Duration::from_secs(7200),async{
                   let mut header=Vec::new();let mut byte=[0u8;1];while !header.ends_with(b"\r\n\r\n"){stream.read_exact(&mut byte).await?;header.push(byte[0]);ensure!(header.len()<16384,"proxy header too large");}
                   let header=std::str::from_utf8(&header)?;let line=header.lines().next().context("empty proxy request")?;let mut parts=line.split_whitespace();let method=parts.next().unwrap_or("");let host=parts.next().unwrap_or("");
                   if method!="CONNECT"||!allowed(host){stream.write_all(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n").await?;return Ok::<_,anyhow::Error>(());}
                   let mut target=tokio::time::timeout(Duration::from_secs(30),TcpStream::connect(host)).await??;stream.write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n").await?;tokio::io::copy_bidirectional(&mut stream,&mut target).await?;Ok(())
                }).await;});},Some(_)=connections.join_next()=>{}}
            }
            connections.abort_all();
        });
        Ok(Self {
            socket,
            token,
            task,
        })
    }
}
impl Drop for Proxy {
    fn drop(&mut self) {
        self.token.cancel();
        self.task.abort();
        let _ = std::fs::remove_file(&self.socket);
        if let Some(dir) = self.socket.parent() {
            let _ = std::fs::remove_dir(dir);
        }
    }
}
/// Runs inside the network namespace. A localhost HTTP proxy bridges to the guarded UNIX socket.
pub async fn bridge(socket: &Path, command: &[String]) -> Result<i32> {
    ensure!(!command.is_empty(), "bridge command missing");
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let url = format!("http://{}", listener.local_addr()?);
    let cancel = CancellationToken::new();
    let token = cancel.clone();
    let socket = socket.to_path_buf();
    let relay = tokio::spawn(async move {
        let mut tasks = tokio::task::JoinSet::new();
        loop {
            tokio::select! {_=token.cancelled()=>break,result=listener.accept()=>{let Ok((mut tcp,_))=result else{break;};let socket=socket.clone();tasks.spawn(async move{if let Ok(mut unix)=UnixStream::connect(socket).await{let _=tokio::io::copy_bidirectional(&mut tcp,&mut unix).await;}});},Some(_)=tasks.join_next()=>{}}
        }
        tasks.abort_all();
    });
    let mut child = tokio::process::Command::new(&command[0]);
    child
        .args(&command[1..])
        .env("HTTPS_PROXY", &url)
        .env("HTTP_PROXY", &url)
        .env("ALL_PROXY", &url)
        .env("NO_PROXY", "")
        .kill_on_drop(true);
    let status = child.status().await?;
    cancel.cancel();
    relay.abort();
    Ok(status.code().unwrap_or(1))
}
pub fn maistro_executable() -> Result<PathBuf> {
    let exe = std::env::current_exe()?;
    if exe.file_name().is_some_and(|s| s == "maistro") {
        return Ok(exe);
    }
    for parent in exe.ancestors().skip(1).take(3) {
        let candidate = parent.join("maistro");
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    anyhow::bail!("maistro executable required beside test/example binaries; run cargo build first")
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn egress_is_only_model_endpoints() {
        assert!(allowed("chatgpt.com:443"));
        for x in [
            "github.com:443",
            "api.github.com:443",
            "chatgpt.com.evil:443",
            "127.0.0.1:443",
            "chatgpt.com:80",
            "auth.openai.com@evil:443",
        ] {
            assert!(!allowed(x));
        }
    }
    #[tokio::test]
    async fn proxy_rejects_github() {
        let t = tempfile::tempdir().unwrap();
        let proxy = Proxy::start(t.path()).unwrap();
        let mut s = UnixStream::connect(&proxy.socket).await.unwrap();
        s.write_all(b"CONNECT api.github.com:443 HTTP/1.1\r\nHost: api.github.com\r\n\r\n")
            .await
            .unwrap();
        let mut bytes = vec![];
        s.read_to_end(&mut bytes).await.unwrap();
        assert!(String::from_utf8(bytes).unwrap().contains("403 Forbidden"));
    }
}
