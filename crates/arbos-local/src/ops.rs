//! The three operations the helper performs. Every op is bounded: commands by a timeout and
//! per-stream output caps, files by the inline cap (bigger ones go to the relay blob store),
//! directory listings by an entry cap.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant, UNIX_EPOCH};

use base64::Engine;
use tokio::io::AsyncReadExt;

use arbos_core::local::{
    LocalDirEntry, LocalOp, LocalResult, LOCAL_FILE_INLINE_MAX, LOCAL_LIST_MAX, LOCAL_OUTPUT_MAX,
};

/// Where an oversized file goes: `PUT {relay_url}/blob/{project_id}/{key}` with the device token.
pub struct BlobSink<'a> {
    pub http: &'a reqwest::Client,
    pub relay_url: &'a str,
    pub token: &'a str,
    pub project_id: &'a str,
}

pub async fn execute(op: &LocalOp, blobs: BlobSink<'_>) -> LocalResult {
    match op {
        LocalOp::Run { cmd, cwd, .. } => run(cmd, cwd.as_deref(), op.timeout_s()).await,
        LocalOp::ReadFile { path, max_bytes } => read_file(path, *max_bytes, blobs).await,
        LocalOp::ListDir { path } => list_dir(path).await,
    }
}

fn expand_home(p: &str) -> PathBuf {
    if let Some(rest) = p.strip_prefix("~/") {
        if let Ok(home) = std::env::var("HOME") {
            return Path::new(&home).join(rest);
        }
    }
    if p == "~" {
        if let Ok(home) = std::env::var("HOME") {
            return PathBuf::from(home);
        }
    }
    PathBuf::from(p)
}

async fn run(cmd: &str, cwd: Option<&str>, timeout_s: u64) -> LocalResult {
    let started = Instant::now();
    let shell = std::env::var("SHELL")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "/bin/sh".into());
    let mut command = tokio::process::Command::new(&shell);
    command
        .arg("-c")
        .arg(cmd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if let Some(dir) = cwd.filter(|d| !d.is_empty()) {
        command.current_dir(expand_home(dir));
    }
    let mut child = match command.spawn() {
        Ok(c) => c,
        Err(e) => {
            return LocalResult::Error {
                message: format!("spawn {shell}: {e}"),
            }
        }
    };
    let mut stdout = child.stdout.take();
    let mut stderr = child.stderr.take();
    let drain = async {
        let (a, b) = tokio::join!(
            capture(
                stdout
                    .as_mut()
                    .map(|s| s as &mut (dyn tokio::io::AsyncRead + Unpin + Send))
            ),
            capture(
                stderr
                    .as_mut()
                    .map(|s| s as &mut (dyn tokio::io::AsyncRead + Unpin + Send))
            )
        );
        let status = child.wait().await;
        (a, b, status)
    };
    match tokio::time::timeout(Duration::from_secs(timeout_s), drain).await {
        Ok(((out, out_trunc), (err, err_trunc), status)) => LocalResult::Run {
            exit: status.ok().and_then(|s| s.code()),
            stdout: out,
            stderr: err,
            stdout_truncated: out_trunc,
            stderr_truncated: err_trunc,
            timed_out: false,
            duration_ms: started.elapsed().as_millis() as u64,
        },
        Err(_) => LocalResult::Run {
            exit: None,
            stdout: String::new(),
            stderr: format!("killed after {timeout_s} s"),
            stdout_truncated: false,
            stderr_truncated: false,
            timed_out: true,
            duration_ms: started.elapsed().as_millis() as u64,
        },
    }
}

/// Read a stream to its end, keeping the first `LOCAL_OUTPUT_MAX` bytes.
async fn capture(stream: Option<&mut (dyn tokio::io::AsyncRead + Unpin + Send)>) -> (String, bool) {
    let Some(stream) = stream else {
        return (String::new(), false);
    };
    let mut kept = Vec::new();
    let mut truncated = false;
    let mut buf = [0u8; 8192];
    loop {
        match stream.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                if kept.len() < LOCAL_OUTPUT_MAX {
                    let room = LOCAL_OUTPUT_MAX - kept.len();
                    kept.extend_from_slice(&buf[..n.min(room)]);
                    if n > room {
                        truncated = true;
                    }
                } else {
                    truncated = true;
                }
            }
        }
    }
    (String::from_utf8_lossy(&kept).into_owned(), truncated)
}

async fn read_file(path: &str, max_bytes: Option<usize>, blobs: BlobSink<'_>) -> LocalResult {
    let p = expand_home(path);
    let meta = match tokio::fs::metadata(&p).await {
        Ok(m) => m,
        Err(e) => {
            return LocalResult::Error {
                message: format!("{}: {e}", p.display()),
            }
        }
    };
    if meta.is_dir() {
        return LocalResult::Error {
            message: format!("{} is a directory (use list_dir)", p.display()),
        };
    }
    let size = meta.len();
    let cap = max_bytes
        .unwrap_or(LOCAL_FILE_INLINE_MAX)
        .min(LOCAL_FILE_INLINE_MAX);
    let bytes = match tokio::fs::read(&p).await {
        Ok(b) => b,
        Err(e) => {
            return LocalResult::Error {
                message: format!("{}: {e}", p.display()),
            }
        }
    };
    let shown = p.display().to_string();
    if bytes.len() <= cap {
        return inline_file(shown, size, bytes, false);
    }
    // Over the inline cap: the relay keeps the full file as a blob; a clipped head goes inline.
    let key = format!("local-{}", ulid::Ulid::new().to_string().to_lowercase());
    let url = format!(
        "{}/blob/{}/{}",
        blobs.relay_url.trim_end_matches('/'),
        blobs.project_id,
        key
    );
    let uploaded = blobs
        .http
        .put(&url)
        .bearer_auth(blobs.token)
        .header("content-type", "application/octet-stream")
        .body(bytes.clone())
        .send()
        .await
        .map(|r| r.status().is_success())
        .unwrap_or(false);
    let head = bytes[..cap].to_vec();
    match inline_file(shown, size, head, true) {
        LocalResult::File {
            path,
            size,
            text,
            bytes_b64,
            truncated,
            ..
        } => LocalResult::File {
            path,
            size,
            text,
            bytes_b64,
            blob: uploaded.then_some(key),
            truncated,
        },
        other => other,
    }
}

fn inline_file(path: String, size: u64, bytes: Vec<u8>, truncated: bool) -> LocalResult {
    let looks_binary = bytes.iter().take(8192).any(|&b| b == 0);
    match (looks_binary, String::from_utf8(bytes)) {
        (false, Ok(text)) => LocalResult::File {
            path,
            size,
            text: Some(text),
            bytes_b64: None,
            blob: None,
            truncated,
        },
        (_, Ok(text)) => LocalResult::File {
            path,
            size,
            text: None,
            bytes_b64: Some(base64::engine::general_purpose::STANDARD.encode(text.as_bytes())),
            blob: None,
            truncated,
        },
        (_, Err(e)) => LocalResult::File {
            path,
            size,
            text: None,
            bytes_b64: Some(base64::engine::general_purpose::STANDARD.encode(e.into_bytes())),
            blob: None,
            truncated,
        },
    }
}

async fn list_dir(path: &str) -> LocalResult {
    let p = expand_home(path);
    let mut rd = match tokio::fs::read_dir(&p).await {
        Ok(r) => r,
        Err(e) => {
            return LocalResult::Error {
                message: format!("{}: {e}", p.display()),
            }
        }
    };
    let mut entries = Vec::new();
    let mut truncated = false;
    while let Ok(Some(ent)) = rd.next_entry().await {
        if entries.len() >= LOCAL_LIST_MAX {
            truncated = true;
            break;
        }
        let ft = ent.file_type().await.ok();
        let meta = ent.metadata().await.ok();
        let kind = match ft {
            Some(t) if t.is_dir() => "dir",
            Some(t) if t.is_file() => "file",
            Some(t) if t.is_symlink() => "symlink",
            Some(_) | None => "other",
        };
        entries.push(LocalDirEntry {
            name: ent.file_name().to_string_lossy().into_owned(),
            kind: kind.to_owned(),
            size: meta.as_ref().filter(|_| kind == "file").map(|m| m.len()),
            modified_ms: meta
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_millis() as i64),
        });
    }
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    LocalResult::Dir {
        path: p.display().to_string(),
        entries,
        truncated,
    }
}
