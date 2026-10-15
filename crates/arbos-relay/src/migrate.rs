//! Migration from one-sandbox-per-project to one-box-per-user (D70). For every legacy sandbox of a
//! user's project: copy its `/work` (repo + `.arbos` state: SQLite, pi and Prime sessions, skills,
//! blobs) into the user box as `/work/<project>`, then stop the old sandbox and mark it
//! `migrated_to`. Idempotent: rows already marked are skipped; a failed project does not stop the
//! others. Old sandboxes are never deleted here.

use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};

use arbos_core::ids::now_ms;

use crate::boxes::{self, Provider};
use crate::daytona::SandboxState;
use crate::state::AppState;

/// One project's outcome.
#[derive(Debug, serde::Serialize)]
pub struct MigrationReport {
    pub project_id: String,
    pub legacy_sandbox: Option<String>,
    pub outcome: String,
    pub work_mib: Option<i64>,
    pub ms: i64,
}

/// Migrate every unmigrated legacy sandbox whose project belongs to `user_id`.
pub async fn migrate_user(state: &AppState, user_id: &str) -> Result<Vec<MigrationReport>> {
    let projects = state.db.projects_of_user(user_id)?;
    let legacy = state.db.legacy_sandboxes()?;
    let mut reports = Vec::new();
    // The target box must exist and run before any copy.
    let provider_default = if state.daytona.is_some() {
        Provider::Daytona
    } else {
        Provider::Process
    };
    let target = boxes::ensure_user_box(state, user_id, provider_default).await?;
    let target_provider =
        Provider::parse(&target.provider).ok_or_else(|| anyhow!("bad target provider"))?;
    if target_provider == Provider::Daytona {
        let dt = state
            .daytona
            .as_ref()
            .ok_or_else(|| anyhow!("DAYTONA_API_KEY not configured"))?;
        let id = target
            .sandbox_id
            .clone()
            .ok_or_else(|| anyhow!("target box has no sandbox id"))?;
        let cur = dt.get(&id).await?;
        if cur.state != SandboxState::Started {
            boxes::ensure_awake(state, user_id, "migration").await;
            dt.wait_for(&id, Duration::from_secs(180), |s| {
                s.state == SandboxState::Started
            })
            .await?;
        }
    }
    for (project_id, legacy_sb, migrated_to) in legacy {
        let t0 = std::time::Instant::now();
        let Some(project) = projects.iter().find(|p| p.id == project_id) else {
            continue; // another user's project (or deleted)
        };
        if migrated_to.is_some() {
            reports.push(MigrationReport {
                project_id: project_id.clone(),
                legacy_sandbox: legacy_sb.sandbox_id.clone(),
                outcome: format!("already migrated to {}", migrated_to.unwrap_or_default()),
                work_mib: None,
                ms: 0,
            });
            continue;
        }
        let res = migrate_one(state, user_id, &project.id, &legacy_sb, &target).await;
        let (outcome, work_mib) = match res {
            Ok(mib) => {
                let _ = state.db.mark_legacy_migrated(&project_id, user_id);
                if let Ok(Some(mut b)) = state.db.user_box(user_id) {
                    if !b.migrated_from.contains(&project_id) {
                        b.migrated_from.push(project_id.clone());
                        let _ = state.db.upsert_box(&b);
                    }
                }
                ("migrated".to_owned(), Some(mib))
            }
            Err(e) => (format!("failed: {e:#}"), None),
        };
        tracing::info!(project = %project_id, outcome = %outcome, "migration step");
        reports.push(MigrationReport {
            project_id,
            legacy_sandbox: legacy_sb.sandbox_id.clone(),
            outcome,
            work_mib,
            ms: t0.elapsed().as_millis() as i64,
        });
    }
    Ok(reports)
}

/// Copy one legacy sandbox's `/work` into the user box's `/work/<project>`. Returns the size in MiB.
async fn migrate_one(
    state: &AppState,
    user_id: &str,
    project_id: &str,
    legacy: &crate::db::Sandbox,
    target: &crate::db::Sandbox,
) -> Result<i64> {
    let legacy_provider =
        Provider::parse(&legacy.provider).ok_or_else(|| anyhow!("bad legacy provider"))?;
    let target_provider =
        Provider::parse(&target.provider).ok_or_else(|| anyhow!("bad target provider"))?;
    match (legacy_provider, target_provider) {
        (Provider::Process, Provider::Process) => {
            // Local: stop the old agentd if it still runs, then copy the directory.
            if let Some(pid) = legacy.local_pid {
                let _ = nix::sys::signal::kill(
                    nix::unistd::Pid::from_raw(pid as i32),
                    nix::sys::signal::Signal::SIGTERM,
                );
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
            let src = state
                .cfg
                .data_dir
                .join("boxes")
                .join(project_id)
                .join("work");
            let dst = state
                .cfg
                .data_dir
                .join("boxes")
                .join(user_id)
                .join("work")
                .join(project_id);
            if !src.exists() {
                return Err(anyhow!("legacy work dir {} is missing", src.display()));
            }
            hold_workspace(state, target, project_id, true).await?;
            let _ = std::fs::remove_dir_all(&dst);
            std::fs::create_dir_all(dst.parent().unwrap())?;
            let st = tokio::process::Command::new("cp")
                .args(["-a", &src.display().to_string(), &dst.display().to_string()])
                .status()
                .await?;
            hold_workspace(state, target, project_id, false).await?;
            if !st.success() {
                return Err(anyhow!("cp -a failed"));
            }
            Ok(dir_size_mib(&dst))
        }
        (Provider::Daytona, Provider::Daytona) => {
            let dt = state
                .daytona
                .as_ref()
                .ok_or_else(|| anyhow!("DAYTONA_API_KEY not configured"))?;
            let old_id = legacy
                .sandbox_id
                .clone()
                .ok_or_else(|| anyhow!("legacy sandbox has no id"))?;
            let new_id = target
                .sandbox_id
                .clone()
                .ok_or_else(|| anyhow!("target box has no sandbox id"))?;
            // 1. The old sandbox must run for the toolbox to read it.
            let cur = dt.get(&old_id).await?;
            if cur.state != SandboxState::Started {
                dt.start(&old_id).await?;
                dt.wait_for(&old_id, Duration::from_secs(180), |s| {
                    s.state == SandboxState::Started
                })
                .await?;
                tokio::time::sleep(Duration::from_secs(3)).await;
            }
            // 2. Size, then one tarball of /work (SQLite side files excluded; they are rebuilt).
            let du = dt
                .toolbox_exec(&old_id, "du -sm /work | cut -f1", 120)
                .await?;
            let mib: i64 = du
                .get("result")
                .and_then(Value::as_str)
                .and_then(|s| s.trim().parse().ok())
                .unwrap_or(0);
            if mib > 3500 {
                return Err(anyhow!(
                    "/work is {mib} MiB; too large to copy through the relay in one piece"
                ));
            }
            let tar = dt
                .toolbox_exec(
                    &old_id,
                    "cd /work && tar czf /tmp/arbos-migrate.tgz --exclude='./.arbos/agentd/*.db-wal' --exclude='./.arbos/agentd/*.db-shm' --exclude='./.arbos/logs' . && ls -l /tmp/arbos-migrate.tgz",
                    900,
                )
                .await?;
            if tar.get("exitCode").and_then(Value::as_i64).unwrap_or(1) != 0 {
                return Err(anyhow!(
                    "tar on the legacy box failed: {}",
                    tar.get("result").and_then(Value::as_str).unwrap_or("")
                ));
            }
            let bytes = dt
                .toolbox_download(&old_id, "/tmp/arbos-migrate.tgz")
                .await
                .context("download tarball")?;
            tracing::info!(project = %project_id, mib, bytes = bytes.len(), "legacy /work packed");
            // 3. Stop the workspace on the user box, replace its directory, release it.
            hold_workspace(state, target, project_id, true).await?;
            let remote_tgz = format!("/tmp/arbos-migrate-{project_id}.tgz");
            let upload = dt.toolbox_upload(&new_id, &remote_tgz, bytes).await;
            let extract = match upload {
                Ok(()) => {
                    dt.toolbox_exec(
                        &new_id,
                        &format!(
                            "rm -rf /work/{p} && mkdir -p /work/{p} && tar xzf {t} -C /work/{p} && rm -f {t} && echo ok",
                            p = project_id,
                            t = remote_tgz
                        ),
                        900,
                    )
                    .await
                }
                Err(e) => Err(e),
            };
            hold_workspace(state, target, project_id, false).await?;
            let out = extract?;
            if out.get("exitCode").and_then(Value::as_i64).unwrap_or(1) != 0 {
                return Err(anyhow!(
                    "extract on the user box failed: {}",
                    out.get("result").and_then(Value::as_str).unwrap_or("")
                ));
            }
            let _ = dt
                .toolbox_exec(&old_id, "rm -f /tmp/arbos-migrate.tgz", 60)
                .await;
            // 4. Hibernate the old sandbox; it stays as a fallback (never deleted here).
            let _ = dt.stop(&old_id, false).await;
            Ok(mib)
        }
        (a, b) => Err(anyhow!(
            "cannot migrate a {} sandbox into a {} box",
            a.as_str(),
            b.as_str()
        )),
    }
}

/// Ask the user box's agentd to stop (hold) or restart (release) one workspace.
async fn hold_workspace(
    state: &AppState,
    target: &crate::db::Sandbox,
    project_id: &str,
    hold: bool,
) -> Result<()> {
    let Some(url) = &target.box_url else {
        return Err(anyhow!("user box has no url yet"));
    };
    let verb = if hold { "hold" } else { "release" };
    let mut req = state
        .http
        .post(format!("{url}/w/{project_id}/{verb}"))
        .bearer_auth(&target.mcp_token)
        .timeout(Duration::from_secs(60));
    if let Some(t) = &target.preview_token {
        req = req.header("x-daytona-preview-token", t);
    }
    let r = req
        .send()
        .await
        .with_context(|| format!("{verb} workspace"))?;
    if !r.status().is_success() {
        return Err(anyhow!("{verb} workspace: HTTP {}", r.status()));
    }
    Ok(())
}

fn dir_size_mib(p: &std::path::Path) -> i64 {
    fn walk(p: &std::path::Path) -> u64 {
        let mut n = 0;
        if let Ok(rd) = std::fs::read_dir(p) {
            for e in rd.flatten() {
                let path = e.path();
                if path.is_dir() {
                    n += walk(&path);
                } else if let Ok(m) = path.metadata() {
                    n += m.len();
                }
            }
        }
        n
    }
    (walk(p) / (1024 * 1024)) as i64
}

/// JSON for the API: the reports plus the box they went to.
pub fn report_json(user_id: &str, reports: &[MigrationReport]) -> Value {
    json!({ "user_id": user_id, "at": now_ms(), "reports": reports })
}
