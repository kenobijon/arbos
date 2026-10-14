//! `arbos-relay daytona-smoke`: the D25 lifecycle check against the live API
//! (`create → started → stop → start → archive → start → delete`) with timings
//! for the D32 cold-start table. Uses an inline Dockerfile when no snapshot is given.

use std::time::{Duration, Instant};

use anyhow::Result;
use serde_json::{json, Value};

use crate::daytona::{CreateSandbox, Daytona, SandboxState};

const INLINE_DOCKERFILE: &str = "FROM node:24-bookworm-slim\nRUN apt-get update && apt-get install -y --no-install-recommends ca-certificates curl git procps && rm -rf /var/lib/apt/lists/*\nRUN npm install -g --ignore-scripts @earendil-works/pi-coding-agent@1.2.0 && npm cache clean --force\nENV PI_TELEMETRY=0 PI_SKIP_VERSION_CHECK=1\n";

pub async fn run(dt: &Daytona, snapshot: Option<String>, keep: bool) -> Result<()> {
    let mut timings: Vec<(String, i64, String)> = Vec::new();
    let mut body = CreateSandbox {
        name: Some(format!(
            "arbos-smoke-{}",
            &arbos_core::ids::ulid()[20..].to_ascii_lowercase()
        )),
        cpu: Some(2),
        memory: Some(4),
        disk: Some(10),
        auto_stop_interval: Some(30),
        auto_archive_interval: Some(7 * 24 * 60),
        auto_delete_interval: Some(-1),
        ..Default::default()
    };
    match &snapshot {
        Some(s) => body.snapshot = Some(s.clone()),
        None => body.build_info = Some(json!({ "dockerfileContent": INLINE_DOCKERFILE })),
    }
    body.labels.insert("arbos.smoke".into(), Value::Bool(true));
    body.env
        .insert("ARBOS_SMOKE".into(), Value::String("1".into()));

    let t = Instant::now();
    let sb = dt.create(&body).await?;
    println!(
        "created {} state={:?} runner={:?} ({} ms)",
        sb.id,
        sb.state,
        sb.runner_id,
        t.elapsed().as_millis()
    );
    let s = dt
        .wait_for(&sb.id, Duration::from_secs(900), |s| {
            s.state == SandboxState::Started
        })
        .await?;
    let create_ms = t.elapsed().as_millis() as i64;
    let how = if snapshot.is_some() {
        "snapshot"
    } else {
        "inline-dockerfile-build"
    };
    println!(
        "started after {create_ms} ms (class={:?}, runner={:?}, autoStop={:?})",
        s.sandbox_class, s.runner_id, s.auto_stop_interval
    );
    timings.push(("create_to_started_ms".into(), create_ms, how.into()));

    let t = Instant::now();
    let v = dt.toolbox_exec(&sb.id, "pi --version 2>&1 || echo no-pi; uname -a; nproc; cat /sys/fs/cgroup/cpu.max 2>/dev/null", 60).await;
    println!(
        "toolbox exec ({} ms): {}",
        t.elapsed().as_millis(),
        v.map(|v| v.to_string().chars().take(300).collect::<String>())
            .unwrap_or_else(|e| e.to_string())
    );

    let t = Instant::now();
    dt.refresh_activity(&sb.id).await?;
    timings.push((
        "last_activity_ms".into(),
        t.elapsed().as_millis() as i64,
        "keep-alive call".into(),
    ));
    let t = Instant::now();
    dt.set_autostop(&sb.id, 0).await?;
    dt.set_autostop(&sb.id, 30).await?;
    timings.push((
        "autostop_toggle_ms".into(),
        t.elapsed().as_millis() as i64,
        "0 then 30".into(),
    ));

    let t = Instant::now();
    let p = dt.preview_url(&sb.id, 7788).await?;
    timings.push((
        "preview_url_ms".into(),
        t.elapsed().as_millis() as i64,
        p.url.clone(),
    ));

    let t = Instant::now();
    dt.stop(&sb.id, false).await?;
    dt.wait_for(&sb.id, Duration::from_secs(300), |s| {
        s.state == SandboxState::Stopped
    })
    .await?;
    timings.push((
        "stop_ms".into(),
        t.elapsed().as_millis() as i64,
        String::new(),
    ));

    let t = Instant::now();
    dt.start(&sb.id).await?;
    dt.wait_for(&sb.id, Duration::from_secs(300), |s| {
        s.state == SandboxState::Started
    })
    .await?;
    timings.push((
        "stopped_to_started_ms".into(),
        t.elapsed().as_millis() as i64,
        String::new(),
    ));

    let t = Instant::now();
    dt.stop(&sb.id, false).await?;
    dt.wait_for(&sb.id, Duration::from_secs(300), |s| {
        s.state == SandboxState::Stopped
    })
    .await?;
    dt.archive(&sb.id).await?;
    dt.wait_for(&sb.id, Duration::from_secs(900), |s| {
        s.state == SandboxState::Archived
    })
    .await?;
    timings.push((
        "stop_and_archive_ms".into(),
        t.elapsed().as_millis() as i64,
        String::new(),
    ));

    let t = Instant::now();
    dt.start(&sb.id).await?;
    dt.wait_for(&sb.id, Duration::from_secs(900), |s| {
        s.state == SandboxState::Started
    })
    .await?;
    timings.push((
        "archived_to_started_ms".into(),
        t.elapsed().as_millis() as i64,
        "~image-only disk".into(),
    ));

    let t = Instant::now();
    dt.update_secrets(&sb.id, vec![])
        .await
        .map(|_| ())
        .unwrap_or_else(|e| println!("updateSecrets({{}}): {e}"));
    timings.push((
        "update_secrets_empty_ms".into(),
        t.elapsed().as_millis() as i64,
        "stop-project revoke".into(),
    ));

    if !keep {
        let t = Instant::now();
        dt.delete(&sb.id).await?;
        timings.push((
            "delete_ms".into(),
            t.elapsed().as_millis() as i64,
            String::new(),
        ));
    } else {
        println!("kept sandbox {}", sb.id);
    }

    println!("\n| timing | ms | note |\n| --- | ---: | --- |");
    for (k, ms, note) in &timings {
        println!("| {k} | {ms} | {note} |");
    }
    Ok(())
}
