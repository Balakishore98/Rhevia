//! Shared harness: runs the real Node signaling server for integration tests.
//!
//! Tests that need it get a fresh server on its own port, killed on drop even
//! if the test panics.

use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use rhevia_link::{ClientInfo, LinkEvent, SignalingClient};

pub fn repo_root() -> PathBuf {
    // CARGO_MANIFEST_DIR is desktop/crates/rhevia-pipeline
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .expect("repo root")
        .to_path_buf()
}

pub struct ServerGuard {
    child: Child,
    pub port: u16,
}

impl ServerGuard {
    pub fn url(&self) -> String {
        format!("ws://127.0.0.1:{}", self.port)
    }
}

impl Drop for ServerGuard {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

pub fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .expect("bind an ephemeral port")
        .local_addr()
        .unwrap()
        .port()
}

/// Starts the real signaling server, or returns None if it has not been built.
pub async fn start_server() -> Option<ServerGuard> {
    let entry = repo_root().join("services/signaling/dist/index.js");
    if !entry.exists() {
        eprintln!(
            "SKIP: {} not found — run `npm install && npm run build` at the repo root",
            entry.display()
        );
        return None;
    }

    let port = free_port();
    let child = Command::new("node")
        .arg(&entry)
        .env("RHEVIA_PORT", port.to_string())
        .env("RHEVIA_HOST", "127.0.0.1")
        .env("RHEVIA_PAIR_URL_BASE", "https://link.test/pair")
        .env("RHEVIA_TURN_URLS", "turn:turn.test:3478")
        .env("RHEVIA_TURN_SECRET", "interop-test-secret")
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("node should be on PATH");

    let guard = ServerGuard { child, port };

    // Poll rather than sleep a fixed amount: startup time varies and a flaky
    // test here would be worse than no test.
    for _ in 0..100 {
        if tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .is_ok()
        {
            return Some(guard);
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("signaling server did not start listening within 5s");
}

pub fn info(name: &str, platform: &str) -> ClientInfo {
    ClientInfo {
        name: name.into(),
        platform: platform.into(),
        app_version: "0.1.0".into(),
    }
}

/// Awaits the next event, failing loudly rather than hanging the suite.
pub async fn next(client: &SignalingClient, what: &str) -> LinkEvent {
    match tokio::time::timeout(Duration::from_secs(5), client.next_event()).await {
        Ok(Some(event)) => event,
        Ok(None) => panic!("signaling closed while waiting for {what}"),
        Err(_) => panic!("timed out waiting for {what}"),
    }
}

use std::process::Stdio as StdioAlias;

/// True if the tool is on PATH.
pub fn have(tool: &str) -> bool {
    Command::new(tool)
        .arg("-version")
        .stdout(StdioAlias::null())
        .stderr(StdioAlias::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Scratch directory under target/, already gitignored.
pub fn workdir() -> PathBuf {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/pipeline-tests");
    std::fs::create_dir_all(&dir).ok();
    dir
}

/// Kills a child process on drop, including on panic.
pub struct Killer(pub Child);
impl Drop for Killer {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
