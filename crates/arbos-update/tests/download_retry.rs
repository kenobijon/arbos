//! The payload download against a stub HTTP server that misbehaves the way the network did on
//! 2026-10-23: a connection that never answers (→ timeout), a body cut short (→ resume with
//! `Range`), a mirror that is down (→ the next URL), then success — and the signature check
//! over the complete bytes at the end.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use arbos_update::appcast::Item;
use arbos_update::client::{download_with, Available};
use arbos_update::fetch::{DownloadFailed, Retry};
use arbos_update::sign::PrivateKey;

/// What the stub does with the n-th connection.
#[derive(Clone, Copy, Debug)]
enum Behaviour {
    /// Accept, read the request, never answer (the client's read timeout must fire).
    Hang,
    /// Answer 200 with the full Content-Length but send only `n` bytes, then close.
    Truncate(usize),
    /// Serve the request properly: 200 for a whole-file request, 206 for a `Range`.
    Serve,
    /// Close the connection at once without a byte.
    Reset,
}

type ProgressLog = Arc<Mutex<Vec<(u64, Option<u64>)>>>;

struct Stub {
    addr: SocketAddr,
    requests: Arc<Mutex<Vec<String>>>,
}

fn fast_retry(attempts: u32) -> Retry {
    Retry {
        attempts,
        base: Duration::from_millis(20),
        cap: Duration::from_millis(80),
        connect_timeout: Duration::from_secs(2),
        read_timeout: Duration::from_millis(400),
    }
}

async fn stub(payload: Vec<u8>, script: Vec<Behaviour>) -> Stub {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let requests: Arc<Mutex<Vec<String>>> = Arc::default();
    let seen = requests.clone();
    let hits = Arc::new(AtomicUsize::new(0));
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            let n = hits.fetch_add(1, Ordering::SeqCst);
            let behaviour = script.get(n).copied().unwrap_or(Behaviour::Serve);
            let payload = payload.clone();
            let seen = seen.clone();
            tokio::spawn(async move {
                if matches!(behaviour, Behaviour::Reset) {
                    drop(sock);
                    return;
                }
                let mut buf = Vec::new();
                let mut tmp = [0u8; 1024];
                while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                    let Ok(k) = sock.read(&mut tmp).await else {
                        return;
                    };
                    if k == 0 {
                        return;
                    }
                    buf.extend_from_slice(&tmp[..k]);
                }
                let head = String::from_utf8_lossy(&buf).to_string();
                seen.lock().unwrap().push(head.clone());
                let range_from = head
                    .lines()
                    .find_map(|l| {
                        l.strip_prefix("range: ")
                            .or_else(|| l.strip_prefix("Range: "))
                    })
                    .and_then(|r| r.trim().strip_prefix("bytes="))
                    .and_then(|r| r.trim_end_matches('-').parse::<usize>().ok());
                match behaviour {
                    Behaviour::Hang => {
                        tokio::time::sleep(Duration::from_secs(30)).await;
                    }
                    Behaviour::Truncate(n) => {
                        let head = format!(
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/octet-stream\r\nConnection: close\r\n\r\n",
                            payload.len()
                        );
                        let _ = sock.write_all(head.as_bytes()).await;
                        let _ = sock.write_all(&payload[..n.min(payload.len())]).await;
                        let _ = sock.flush().await;
                        let _ = sock.shutdown().await;
                    }
                    Behaviour::Serve => {
                        let (status, from) = match range_from {
                            Some(f) if f < payload.len() => ("206 Partial Content", f),
                            Some(_) => {
                                let _ = sock
                                    .write_all(
                                        format!(
                                            "HTTP/1.1 416 Range Not Satisfiable\r\nContent-Range: bytes */{}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                                            payload.len()
                                        )
                                        .as_bytes(),
                                    )
                                    .await;
                                return;
                            }
                            None => ("200 OK", 0),
                        };
                        let body = &payload[from..];
                        let mut head = format!(
                            "HTTP/1.1 {status}\r\nContent-Length: {}\r\nContent-Type: application/octet-stream\r\nConnection: close\r\n",
                            body.len()
                        );
                        if from > 0 {
                            head.push_str(&format!(
                                "Content-Range: bytes {from}-{}/{}\r\n",
                                payload.len() - 1,
                                payload.len()
                            ));
                        }
                        head.push_str("\r\n");
                        let _ = sock.write_all(head.as_bytes()).await;
                        let _ = sock.write_all(body).await;
                        let _ = sock.flush().await;
                        let _ = sock.shutdown().await;
                    }
                    Behaviour::Reset => unreachable!(),
                }
            });
        }
    });
    Stub { addr, requests }
}

fn payload() -> Vec<u8> {
    // 300 KiB, not compressible, not all one byte: a wrong offset on resume would show.
    let mut x: u32 = 0x9e37_79b9;
    (0..300 * 1024)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            (x & 0xff) as u8
        })
        .collect()
}

fn available(bytes: &[u8], key: &PrivateKey, url: String) -> Available {
    Available {
        item: Item {
            title: "0.6.174".into(),
            version: "0.6.174".into(),
            build: "1".into(),
            ..Default::default()
        },
        url,
        length: bytes.len() as u64,
        ed_signature: key.sign(bytes),
    }
}

#[tokio::test]
async fn hang_then_truncate_then_resume_and_verify() {
    let bytes = payload();
    let key = PrivateKey::generate();
    let cut = 100 * 1024;
    let stub = stub(
        bytes.clone(),
        vec![Behaviour::Hang, Behaviour::Truncate(cut), Behaviour::Serve],
    )
    .await;
    let url = format!("http://{}/Arbos-0.6.174-macos-universal.tar.gz", stub.addr);
    let avail = available(&bytes, &key, url.clone());
    let dir = tempdir("resume");
    let progress_log: ProgressLog = Arc::default();
    let plog = progress_log.clone();
    let progress: arbos_update::client::ProgressFn = Arc::new(move |got, total| {
        plog.lock().unwrap().push((got, total));
    });

    let started = std::time::Instant::now();
    let downloaded = download_with(
        &avail,
        &key.public(),
        &dir,
        Some(progress),
        &fast_retry(5),
        std::slice::from_ref(&url),
    )
    .await
    .expect("the third attempt completes the file");
    assert!(started.elapsed() < Duration::from_secs(10));
    assert_eq!(downloaded.attempts, 3);
    assert_eq!(downloaded.url, url);
    let path = downloaded.path;

    assert_eq!(
        std::fs::read(&path).unwrap(),
        bytes,
        "verified bytes on disk"
    );
    assert!(!dir
        .join("Arbos-0.6.174-macos-universal.tar.gz.part")
        .exists());

    let requests = stub.requests.lock().unwrap().clone();
    assert_eq!(
        requests.len(),
        3,
        "hang, truncated 200, resumed 206: {requests:#?}"
    );
    assert!(
        !requests[0].to_ascii_lowercase().contains("range:"),
        "first request is whole-file"
    );
    assert!(
        !requests[1].to_ascii_lowercase().contains("range:"),
        "nothing on disk yet after the hang"
    );
    assert!(
        requests[2]
            .to_ascii_lowercase()
            .contains(&format!("range: bytes={cut}-")),
        "third request resumes from the cut: {}",
        requests[2]
    );

    let log = progress_log.lock().unwrap().clone();
    let total = Some(bytes.len() as u64);
    assert!(
        log.iter().all(|(_, t)| *t == total),
        "total is known throughout: {log:?}"
    );
    assert_eq!(log.last().unwrap().0, bytes.len() as u64);
    // The resumed attempt starts its progress at the resume offset, never back at zero.
    let resume_idx = log
        .iter()
        .position(|(g, _)| *g == cut as u64)
        .expect("progress at the cut");
    assert!(log[resume_idx..].windows(2).all(|w| w[0].0 <= w[1].0));
    cleanup(&dir);
}

#[tokio::test]
async fn mirror_down_falls_back_to_second_url() {
    let bytes = payload();
    let key = PrivateKey::generate();
    let good = stub(bytes.clone(), vec![Behaviour::Serve]).await;
    // A port nobody listens on: connection refused at once.
    let closed = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let dead = closed.local_addr().unwrap();
    drop(closed);
    let mirror = format!("http://{dead}/Arbos-0.6.174-macos-universal.tar.gz");
    let github = format!("http://{}/Arbos-0.6.174-macos-universal.tar.gz", good.addr);
    let avail = available(&bytes, &key, github.clone());
    let dir = tempdir("fallback");
    let downloaded = download_with(
        &avail,
        &key.public(),
        &dir,
        None,
        &fast_retry(3),
        &[mirror, github.clone()],
    )
    .await
    .expect("second URL serves the file");
    assert_eq!(std::fs::read(&downloaded.path).unwrap(), bytes);
    assert_eq!((downloaded.attempts, downloaded.url), (2, github));
    assert_eq!(good.requests.lock().unwrap().len(), 1);
    cleanup(&dir);
}

#[tokio::test]
async fn gives_up_in_plain_words_after_all_attempts() {
    let bytes = payload();
    let key = PrivateKey::generate();
    let stub = stub(bytes.clone(), vec![Behaviour::Reset; 8]).await;
    let url = format!("http://{}/Arbos-0.6.174-macos-universal.tar.gz", stub.addr);
    let avail = available(&bytes, &key, url.clone());
    let dir = tempdir("giveup");
    let err = download_with(&avail, &key.public(), &dir, None, &fast_retry(3), &[url])
        .await
        .expect_err("every attempt is reset");
    let failed = err
        .downcast_ref::<DownloadFailed>()
        .expect("a DownloadFailed, not a raw transport error");
    assert_eq!(failed.attempts, 3);
    let plain = failed.to_string();
    assert!(
        plain.starts_with("Could not download from 127.0.0.1:"),
        "{plain}"
    );
    assert!(plain.contains(" after 3 tries ("), "{plain}");
    assert!(plain.ends_with(")."), "{plain}");
    assert!(
        !plain.contains("reqwest") && !plain.contains("hyper") && !plain.contains("os error"),
        "{plain}"
    );
    assert_eq!(failed.detail().lines().count(), 3);
    cleanup(&dir);
}

#[tokio::test]
async fn tampered_bytes_are_rejected_even_when_complete() {
    let bytes = payload();
    let key = PrivateKey::generate();
    let mut wrong = bytes.clone();
    wrong[12_345] ^= 0x55;
    let stub = stub(wrong, vec![Behaviour::Serve]).await;
    let url = format!("http://{}/Arbos-0.6.174-macos-universal.tar.gz", stub.addr);
    let avail = available(&bytes, &key, url.clone());
    let dir = tempdir("tamper");
    let err = download_with(&avail, &key.public(), &dir, None, &fast_retry(2), &[url])
        .await
        .expect_err("signature over the wrong bytes");
    assert!(format!("{err:#}").contains("signature"), "{err:#}");
    assert!(
        std::fs::read_dir(&dir).unwrap().next().is_none(),
        "nothing unverified left on disk"
    );
    cleanup(&dir);
}

fn tempdir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "arbos-update-test-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn cleanup(dir: &std::path::Path) {
    let _ = std::fs::remove_dir_all(dir);
}

/// A mirror that answers 200 with the wrong file (a web page where the tarball should be — what a
/// not-yet-configured site does) must not poison the download: its bytes are rejected on size
/// and nothing of them is resumed from on the next host.
#[tokio::test]
async fn wrong_sized_mirror_response_is_not_resumed_from() {
    let bytes = payload();
    let key = PrivateKey::generate();
    let page = b"<!doctype html><html><body>Arbos</body></html>".to_vec();
    let mirror = stub(page, vec![Behaviour::Serve; 4]).await;
    let github = stub(bytes.clone(), vec![Behaviour::Serve]).await;
    let mirror_url = format!(
        "http://{}/Arbos-0.6.174-macos-universal.tar.gz",
        mirror.addr
    );
    let github_url = format!(
        "http://{}/Arbos-0.6.174-macos-universal.tar.gz",
        github.addr
    );
    let avail = available(&bytes, &key, github_url.clone());
    let dir = tempdir("wrongsize");
    let downloaded = download_with(
        &avail,
        &key.public(),
        &dir,
        None,
        &fast_retry(4),
        &[mirror_url, github_url.clone()],
    )
    .await
    .expect("GitHub serves the real file on attempt 2");
    assert_eq!((downloaded.attempts, downloaded.url), (2, github_url));
    assert_eq!(std::fs::read(&downloaded.path).unwrap(), bytes);
    let gh = github.requests.lock().unwrap().clone();
    assert_eq!(gh.len(), 1);
    assert!(
        !gh[0].to_ascii_lowercase().contains("range:"),
        "started over, did not resume from the web page: {}",
        gh[0]
    );
    cleanup(&dir);
}
