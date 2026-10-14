//! Daytona REST client (D25, D59): the calls the relay uses, typed from the live
//! OpenAPI document (`https://www.daytona.io/docs/openapi.json`, fetched
//! 2026-10-14; operationIds noted on each method). Base URL `https://app.daytona.io/api`.
//! The API key lives only here (D2, D28). The full call set is kept even where M0 does not use it yet.
#![allow(dead_code)]

use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Clone)]
pub struct Daytona {
    http: reqwest::Client,
    base: String,
    key: String,
    org: Option<String>,
}

/// `components.schemas.SandboxState` (live spec enum, 22 values).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SandboxState {
    Creating,
    Restoring,
    Destroyed,
    Destroying,
    Started,
    Stopped,
    Starting,
    Stopping,
    Error,
    BuildFailed,
    PendingBuild,
    BuildingSnapshot,
    Unknown,
    PullingSnapshot,
    Archived,
    Archiving,
    Resizing,
    Snapshotting,
    Forking,
    Pausing,
    Paused,
    Resuming,
}

impl SandboxState {
    /// Arbos's hibernate mapping (design §4.6).
    pub fn arbos_state(self) -> &'static str {
        match self {
            SandboxState::Started => "awake",
            SandboxState::Creating
            | SandboxState::Restoring
            | SandboxState::Starting
            | SandboxState::PullingSnapshot
            | SandboxState::PendingBuild
            | SandboxState::BuildingSnapshot
            | SandboxState::Resuming
            | SandboxState::Forking
            | SandboxState::Snapshotting
            | SandboxState::Resizing => "waking",
            SandboxState::Stopped
            | SandboxState::Stopping
            | SandboxState::Paused
            | SandboxState::Pausing => "hibernated_light",
            SandboxState::Archived | SandboxState::Archiving => "hibernated_deep",
            SandboxState::Destroyed | SandboxState::Destroying => "deleted",
            SandboxState::Error | SandboxState::BuildFailed | SandboxState::Unknown => "error",
        }
    }
}

/// Subset of `components.schemas.Sandbox`.
#[derive(Debug, Clone, Deserialize)]
pub struct Sandbox {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    pub state: SandboxState,
    #[serde(rename = "desiredState", default)]
    pub desired_state: Option<SandboxState>,
    #[serde(rename = "errorReason", default)]
    pub error_reason: Option<String>,
    #[serde(default)]
    pub snapshot: Option<String>,
    #[serde(default)]
    pub target: Option<String>,
    #[serde(rename = "autoStopInterval", default)]
    pub auto_stop_interval: Option<f64>,
    #[serde(rename = "autoArchiveInterval", default)]
    pub auto_archive_interval: Option<f64>,
    #[serde(rename = "lastActivityAt", default)]
    pub last_activity_at: Option<String>,
    #[serde(rename = "sandboxClass", default)]
    pub sandbox_class: Option<String>,
    #[serde(rename = "runnerId", default)]
    pub runner_id: Option<String>,
}

/// `components.schemas.CreateSandbox` (the fields we set).
#[derive(Debug, Clone, Serialize, Default)]
pub struct CreateSandbox {
    pub name: Option<String>,
    pub snapshot: Option<String>,
    pub env: serde_json::Map<String, Value>,
    pub labels: serde_json::Map<String, Value>,
    pub cpu: Option<u32>,
    pub memory: Option<u32>,
    pub disk: Option<u32>,
    #[serde(rename = "autoStopInterval")]
    pub auto_stop_interval: Option<i64>,
    #[serde(rename = "autoArchiveInterval")]
    pub auto_archive_interval: Option<i64>,
    #[serde(rename = "autoDeleteInterval")]
    pub auto_delete_interval: Option<i64>,
    #[serde(rename = "ttlMinutes")]
    pub ttl_minutes: Option<i64>,
    /// Each entry maps ENV_VAR → vault secret name (placeholder secrets, D28).
    pub secrets: Vec<serde_json::Map<String, Value>>,
    /// `buildInfo.dockerfileContent`: inline Dockerfile for the declarative builder.
    #[serde(rename = "buildInfo", skip_serializing_if = "Option::is_none")]
    pub build_info: Option<Value>,
}

/// `components.schemas.PortPreviewUrl`.
#[derive(Debug, Clone, Deserialize)]
pub struct PortPreviewUrl {
    pub url: String,
    pub token: String,
}

/// `components.schemas.SignedPortPreviewUrl`.
#[derive(Debug, Clone, Deserialize)]
pub struct SignedPortPreviewUrl {
    pub url: String,
    pub token: String,
    pub port: i64,
}

/// `components.schemas.SnapshotDto` (subset).
#[derive(Debug, Clone, Deserialize)]
pub struct Snapshot {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub state: Option<String>,
    #[serde(rename = "errorReason", default)]
    pub error_reason: Option<String>,
}

impl Daytona {
    pub fn new(key: String, base: Option<String>, org: Option<String>) -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(120))
            .build()
            .expect("client");
        Self {
            http,
            base: base.unwrap_or_else(|| "https://app.daytona.io/api".to_owned()),
            key,
            org,
        }
    }

    fn req(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        let mut r = self
            .http
            .request(method, format!("{}{}", self.base, path))
            .bearer_auth(&self.key);
        if let Some(org) = &self.org {
            r = r.header("X-Daytona-Organization-ID", org);
        }
        r
    }

    async fn send<T: for<'de> Deserialize<'de>>(
        &self,
        r: reqwest::RequestBuilder,
        what: &str,
    ) -> Result<T> {
        let resp = r
            .send()
            .await
            .with_context(|| format!("daytona {what}: request"))?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(anyhow!(
                "daytona {what}: HTTP {status}: {}",
                text.chars().take(400).collect::<String>()
            ));
        }
        serde_json::from_str(&text).with_context(|| {
            format!(
                "daytona {what}: decode {}",
                text.chars().take(200).collect::<String>()
            )
        })
    }

    /// `createSandbox` — `POST /sandbox`.
    pub async fn create(&self, body: &CreateSandbox) -> Result<Sandbox> {
        self.send(
            self.req(reqwest::Method::POST, "/sandbox").json(body),
            "create",
        )
        .await
    }

    /// `getSandbox` — `GET /sandbox/{sandboxIdOrName}`.
    pub async fn get(&self, id: &str) -> Result<Sandbox> {
        self.send(
            self.req(reqwest::Method::GET, &format!("/sandbox/{id}")),
            "get",
        )
        .await
    }

    /// `startSandbox` — `POST /sandbox/{sandboxIdOrName}/start`.
    pub async fn start(&self, id: &str) -> Result<Sandbox> {
        self.send(
            self.req(reqwest::Method::POST, &format!("/sandbox/{id}/start")),
            "start",
        )
        .await
    }

    /// `stopSandbox` — `POST /sandbox/{sandboxIdOrName}/stop?force=`.
    pub async fn stop(&self, id: &str, force: bool) -> Result<Sandbox> {
        self.send(
            self.req(reqwest::Method::POST, &format!("/sandbox/{id}/stop"))
                .query(&[("force", force)]),
            "stop",
        )
        .await
    }

    /// `archiveSandbox` — `POST /sandbox/{sandboxIdOrName}/archive`.
    pub async fn archive(&self, id: &str) -> Result<Sandbox> {
        self.send(
            self.req(reqwest::Method::POST, &format!("/sandbox/{id}/archive")),
            "archive",
        )
        .await
    }

    /// `deleteSandbox` — `DELETE /sandbox/{sandboxIdOrName}`.
    pub async fn delete(&self, id: &str) -> Result<Sandbox> {
        self.send(
            self.req(reqwest::Method::DELETE, &format!("/sandbox/{id}")),
            "delete",
        )
        .await
    }

    /// `setAutostopInterval` — `POST /sandbox/{sandboxIdOrName}/autostop/{interval}` (minutes, 0 = off).
    pub async fn set_autostop(&self, id: &str, minutes: i64) -> Result<Sandbox> {
        self.send(
            self.req(
                reqwest::Method::POST,
                &format!("/sandbox/{id}/autostop/{minutes}"),
            ),
            "autostop",
        )
        .await
    }

    /// `updateLastActivity` — `POST /sandbox/{sandboxId}/last-activity` (the keep-alive, design §4.6).
    pub async fn refresh_activity(&self, id: &str) -> Result<()> {
        let r = self
            .req(
                reqwest::Method::POST,
                &format!("/sandbox/{id}/last-activity"),
            )
            .json(&json!({ "activityType": "arbos-keepalive" }));
        let resp = r.send().await.context("daytona last-activity")?;
        if !resp.status().is_success() {
            return Err(anyhow!("daytona last-activity: HTTP {}", resp.status()));
        }
        Ok(())
    }

    /// `updateSandboxSecrets` — `PUT /sandbox/{sandboxIdOrName}/secrets` (`{}` on Stop project, D28).
    pub async fn update_secrets(
        &self,
        id: &str,
        secrets: Vec<serde_json::Map<String, Value>>,
    ) -> Result<Sandbox> {
        self.send(
            self.req(reqwest::Method::PUT, &format!("/sandbox/{id}/secrets"))
                .json(&json!({ "secrets": secrets })),
            "secrets",
        )
        .await
    }

    /// `getPortPreviewUrl` — `GET /sandbox/{sandboxIdOrName}/ports/{port}/preview-url` (sandbox-wide token; relay only).
    pub async fn preview_url(&self, id: &str, port: u16) -> Result<PortPreviewUrl> {
        self.send(
            self.req(
                reqwest::Method::GET,
                &format!("/sandbox/{id}/ports/{port}/preview-url"),
            ),
            "preview-url",
        )
        .await
    }

    /// `getSignedPortPreviewUrl` — per-port, expiring (v2 direct path, D24).
    pub async fn signed_preview_url(
        &self,
        id: &str,
        port: u16,
        expires_in_seconds: u32,
    ) -> Result<SignedPortPreviewUrl> {
        self.send(
            self.req(
                reqwest::Method::GET,
                &format!("/sandbox/{id}/ports/{port}/signed-preview-url"),
            )
            .query(&[("expiresInSeconds", expires_in_seconds)]),
            "signed-preview-url",
        )
        .await
    }

    /// `createSnapshot` — `POST /snapshots` from a registry image.
    pub async fn create_snapshot(
        &self,
        name: &str,
        image: &str,
        cpu: u32,
        memory: u32,
        disk: u32,
        entrypoint: Option<Vec<String>>,
    ) -> Result<Snapshot> {
        let mut body =
            json!({ "name": name, "imageName": image, "cpu": cpu, "memory": memory, "disk": disk });
        if let Some(e) = entrypoint {
            body["entrypoint"] = json!(e);
        }
        self.send(
            self.req(reqwest::Method::POST, "/snapshots").json(&body),
            "create-snapshot",
        )
        .await
    }

    /// `getSnapshot` — `GET /snapshots/{id}`.
    pub async fn get_snapshot(&self, id_or_name: &str) -> Result<Snapshot> {
        self.send(
            self.req(reqwest::Method::GET, &format!("/snapshots/{id_or_name}")),
            "get-snapshot",
        )
        .await
    }

    /// Poll `getSandbox` until `pred` holds or `timeout` passes.
    pub async fn wait_for(
        &self,
        id: &str,
        timeout: Duration,
        pred: impl Fn(&Sandbox) -> bool,
    ) -> Result<Sandbox> {
        let start = std::time::Instant::now();
        loop {
            let s = self.get(id).await?;
            if pred(&s) {
                return Ok(s);
            }
            if matches!(s.state, SandboxState::Error | SandboxState::BuildFailed) {
                return Err(anyhow!(
                    "sandbox {id} entered {:?}: {}",
                    s.state,
                    s.error_reason.unwrap_or_default()
                ));
            }
            if start.elapsed() > timeout {
                return Err(anyhow!(
                    "sandbox {id} still {:?} after {:?}",
                    s.state,
                    timeout
                ));
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }

    // ---- toolbox (runner-injected HTTP API inside the sandbox; sandbox-image.md §1) -----------

    /// `POST /toolbox/{sandboxId}/toolbox/files/upload?path=` — used by `--local-box=daytona-toolbox` to
    /// place `arbos-agentd` in a sandbox built from an inline Dockerfile.
    pub async fn toolbox_upload(&self, id: &str, path: &str, bytes: Vec<u8>) -> Result<()> {
        let part = reqwest::multipart::Part::bytes(bytes).file_name("file");
        let form = reqwest::multipart::Form::new().part("file", part);
        let resp = self
            .req(
                reqwest::Method::POST,
                &format!("/toolbox/{id}/toolbox/files/upload"),
            )
            .query(&[("path", path)])
            .multipart(form)
            .send()
            .await
            .context("toolbox upload")?;
        if !resp.status().is_success() {
            return Err(anyhow!(
                "toolbox upload: HTTP {} {}",
                resp.status(),
                resp.text().await.unwrap_or_default()
            ));
        }
        Ok(())
    }

    /// `POST /toolbox/{sandboxId}/toolbox/process/execute`.
    pub async fn toolbox_exec(&self, id: &str, command: &str, timeout_s: u64) -> Result<Value> {
        self.send(
            self.req(
                reqwest::Method::POST,
                &format!("/toolbox/{id}/toolbox/process/execute"),
            )
            .json(&json!({ "command": command, "timeout": timeout_s })),
            "toolbox exec",
        )
        .await
    }

    /// `POST /toolbox/{sandboxId}/toolbox/process/session` then `.../session/{sessionId}/exec` with `runAsync`.
    pub async fn toolbox_session_exec_async(
        &self,
        id: &str,
        session: &str,
        command: &str,
    ) -> Result<Value> {
        let _ = self
            .req(
                reqwest::Method::POST,
                &format!("/toolbox/{id}/toolbox/process/session"),
            )
            .json(&json!({ "sessionId": session }))
            .send()
            .await;
        self.send(
            self.req(
                reqwest::Method::POST,
                &format!("/toolbox/{id}/toolbox/process/session/{session}/exec"),
            )
            .json(&json!({ "command": command, "runAsync": true })),
            "toolbox session exec",
        )
        .await
    }
}
