//! Standalone adapter example, not a nain extension or panel.
//! cargo run --example nain_adapter -- /workspace QUERY --agx /absolute/path/agx
use anyhow::{Context, Result};
use clap::{Parser, ValueEnum};
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc,
};

#[derive(Clone, ValueEnum)]
enum Mode {
    Text,
    Symbol,
    Ranked,
}
impl Mode {
    fn name(&self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Symbol => "symbol",
            Self::Ranked => "ranked",
        }
    }
}
#[derive(Parser)]
struct Args {
    root: PathBuf,
    query: String,
    #[arg(long)]
    agx: Option<PathBuf>,
    #[arg(long, value_enum, default_value = "symbol")]
    mode: Mode,
}
fn executable(path: &Path) -> bool {
    let Ok(metadata) = path.metadata() else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}
/// Explicit configuration wins; invalid explicit paths fail instead of silently changing binaries.
fn locate(explicit: Option<&Path>) -> Result<PathBuf> {
    if let Some(path) = explicit {
        anyhow::ensure!(
            path.is_absolute() && executable(path),
            "agx path must be an absolute executable file"
        );
        return Ok(path.canonicalize()?);
    }
    let mut candidates = std::env::var_os("PATH")
        .map(|p| {
            std::env::split_paths(&p)
                .map(|d| d.join("agx"))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if let Some(base) = directories::BaseDirs::new() {
        candidates.extend([
            base.home_dir().join(".cargo/bin/agx"),
            base.home_dir().join(".local/bin/agx"),
        ]);
    }
    candidates.extend([
        PathBuf::from("/opt/homebrew/bin/agx"),
        PathBuf::from("/usr/local/bin/agx"),
    ]);
    candidates.into_iter().find(|p|executable(p)).context("agx not found; install with cargo install --path . --locked or configure an absolute executable path")?.canonicalize().map_err(Into::into)
}
/// UI integrations should drive send/receive asynchronously; never block a GPUI/UI thread.
pub struct WorkerClient {
    child: Child,
    input: Option<ChildStdin>,
    messages: mpsc::Receiver<Result<Value>>,
    next: u64,
}
impl WorkerClient {
    fn spawn(executable: &Path) -> Result<Self> {
        let mut child = Command::new(executable)
            .args(["serve", "--stdio", "--restricted"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()?;
        let input = child.stdin.take();
        let output = child.stdout.take().context("stdout unavailable")?;
        let (tx, messages) = mpsc::sync_channel(64);
        std::thread::spawn(move || {
            let mut output = BufReader::new(output);
            loop {
                let mut bytes = Vec::new();
                let line = std::io::Read::by_ref(&mut output)
                    .take(4 * 1024 * 1024 + 1)
                    .read_until(b'\n', &mut bytes);
                match line {
                    Ok(0) => break,
                    Ok(_) if bytes.len() <= 4 * 1024 * 1024 => {
                        if tx
                            .send(serde_json::from_slice(&bytes).map_err(Into::into))
                            .is_err()
                        {
                            break;
                        }
                    }
                    Ok(_) => {
                        let _ =
                            tx.send(Err(anyhow::anyhow!("worker response frame exceeded 4 MiB")));
                        break;
                    }
                    Err(e) => {
                        let _ = tx.send(Err(e.into()));
                        break;
                    }
                }
            }
        });
        Ok(Self {
            child,
            input,
            messages,
            next: 0,
        })
    }
    fn send(&mut self, method: &str, params: Value) -> Result<String> {
        self.next += 1;
        let id = format!("nain-{}", self.next);
        self.write(json!({"id":id,"method":method,"params":params}))?;
        Ok(id)
    }
    fn write(&mut self, value: Value) -> Result<()> {
        let input = self.input.as_mut().context("worker is closed")?;
        serde_json::to_writer(&mut *input, &value)?;
        input.write_all(b"\n")?;
        input.flush()?;
        Ok(())
    }
    pub fn cancel(&mut self, request_id: &str) -> Result<()> {
        self.write(json!({"method":"cancel","params":{"request_id":request_id}}))
    }
    fn receive(&self) -> Result<Value> {
        let message = self
            .messages
            .recv_timeout(std::time::Duration::from_secs(30))
            .context("worker closed or timed out")??;
        anyhow::ensure!(
            message["protocol_version"] == 1,
            "unsupported worker protocol"
        );
        Ok(message)
    }
    fn wait(&self, id: &str) -> Result<Value> {
        loop {
            let message = self.receive()?;
            if message["id"] == id {
                if let Some(error) = message.get("error") {
                    anyhow::bail!("worker error: {error}");
                }
                return Ok(message["result"].clone());
            }
            if message["method"] == "index/progress" {
                eprintln!("{}", message["params"]);
            } else {
                anyhow::bail!("unexpected response id; multiplex by id in a concurrent adapter");
            }
        }
    }
}
impl Drop for WorkerClient {
    fn drop(&mut self) {
        self.input.take();
        // A UI adapter should request graceful EOF and wait off-thread, then kill after a timeout.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
fn main() -> Result<()> {
    let args = Args::parse();
    let path = locate(args.agx.as_deref())?;
    let mut worker = WorkerClient::spawn(&path)?;
    let init=worker.send("initialize",json!({"protocol_version":1,"workspace_id":"nain-example","restricted":true,"roots":[{"id":"root","path":args.root}]}))?;
    let capabilities = worker.wait(&init)?;
    anyhow::ensure!(
        capabilities["restricted"] == true
            && capabilities["search_modes"] == json!(["text", "symbol", "ranked"]),
        "unexpected worker capabilities"
    );
    let index = worker.send("index/refresh", json!({"root_id":"root"}))?;
    let status = worker.wait(&index)?;
    let request=worker.send("search",json!({"root_id":"root","mode":args.mode.name(),"query":args.query,"expected_index_version":status["index_version"]}))?;
    let result = worker.wait(&request)?;
    anyhow::ensure!(
        result["schema_version"] == 2 && result["session_id"] == capabilities["session_id"],
        "stale or unsupported result"
    );
    println!("{}", result);
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn explicit_paths_with_spaces_are_not_shell_commands() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agx with spaces; literal");
        std::fs::write(&path, b"fixture").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(locate(Some(&path)).unwrap(), path.canonicalize().unwrap());
        assert!(locate(Some(Path::new("relative/agx"))).is_err());
        assert!(locate(Some(&dir.path().join("missing"))).is_err());
    }
}
