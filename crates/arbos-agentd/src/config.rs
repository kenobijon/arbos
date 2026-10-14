//! Box configuration from the environment the relay injects (sandbox-image.md §4).

use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct Config {
    pub project_id: String,
    pub project_name: String,
    pub relay_url: String,
    pub box_token: String,
    pub mcp_token: String,
    pub state_dir: PathBuf,
    pub work_dir: PathBuf,
    pub bind: String,
    pub port: u16,
    pub mcp_port: u16,
    pub hook_port: u16,
    pub proxy_port: u16,
    pub talking_model: String,
    pub subagent_model: String,
    pub decision_model: String,
    pub instructions: String,
    pub pi_bin: String,
    pub prime_bin: String,
    pub claude_bin: String,
    pub pi_ext_path: PathBuf,
    pub local_box: bool,
    pub device_id: String,
}

fn env_or(k: &str, d: &str) -> String {
    std::env::var(k)
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| d.to_owned())
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        let project_id = std::env::var("ARBOS_PROJECT_ID")
            .map_err(|_| anyhow::anyhow!("ARBOS_PROJECT_ID is required"))?;
        let state_dir = PathBuf::from(env_or("ARBOS_STATE_DIR", "/work/.arbos"));
        let work_dir = PathBuf::from(env_or("ARBOS_WORK_DIR", "/work"));
        let base_port: u16 = env_or("ARBOS_PORT", "7788").parse().unwrap_or(7788);
        let sandbox_id = std::env::var("DAYTONA_SANDBOX_ID").ok();
        let local_box = env_or("ARBOS_LOCAL_BOX", "0") == "1";
        let device_id = match &sandbox_id {
            Some(id) => format!("box:{id}"),
            None => format!("box:local-{project_id}"),
        };
        let pi_ext_path = std::env::var("ARBOS_PI_EXT")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                state_dir
                    .join("pi")
                    .join("extensions")
                    .join("arbos-pi-ext")
                    .join("index.ts")
            });
        Ok(Self {
            project_name: env_or("ARBOS_PROJECT_NAME", &project_id),
            relay_url: env_or("ARBOS_RELAY_URL", "http://127.0.0.1:7700")
                .trim_end_matches('/')
                .to_owned(),
            box_token: env_or("ARBOS_BOX_TOKEN", ""),
            mcp_token: env_or("ARBOS_MCP_TOKEN", ""),
            bind: env_or("ARBOS_BIND", "0.0.0.0"),
            port: base_port,
            // Local boxes share one host, so the loopback ports hang off the main port.
            mcp_port: if local_box { base_port + 1 } else { 7789 },
            hook_port: if local_box { base_port + 2 } else { 7790 },
            proxy_port: if local_box { base_port + 3 } else { 7791 },
            talking_model: env_or("ARBOS_TALKING_MODEL", "anthropic/claude-haiku-5.5"),
            subagent_model: env_or("ARBOS_SUBAGENT_MODEL", "anthropic/claude-sonnet-5.5"),
            decision_model: env_or(
                "ARBOS_DECISION_MODEL",
                &env_or("ARBOS_TALKING_MODEL", "anthropic/claude-haiku-5.5"),
            ),
            instructions: env_or("ARBOS_INSTRUCTIONS", ""),
            pi_bin: env_or("ARBOS_PI_BIN", "pi"),
            prime_bin: env_or("ARBOS_PRIME_BIN", "prime-agent"),
            claude_bin: env_or("ARBOS_CLAUDE_BIN", "claude"),
            pi_ext_path,
            local_box,
            device_id,
            project_id,
            state_dir,
            work_dir,
        })
    }

    pub fn proxy_base(&self) -> String {
        format!("http://127.0.0.1:{}", self.proxy_port)
    }

    pub fn hook_base(&self) -> String {
        format!("http://127.0.0.1:{}", self.hook_port)
    }

    pub fn mcp_url(&self) -> String {
        format!("http://127.0.0.1:{}/mcp", self.mcp_port)
    }

    pub fn pi_dir(&self) -> PathBuf {
        self.state_dir.join("pi")
    }

    pub fn pi_sessions_dir(&self) -> PathBuf {
        self.pi_dir().join("sessions")
    }

    pub fn prime_dir(&self) -> PathBuf {
        self.state_dir.join("prime")
    }

    pub fn blobs_dir(&self) -> PathBuf {
        self.state_dir.join("blobs")
    }

    pub fn audit_path(&self) -> PathBuf {
        self.state_dir.join("audit.jsonl")
    }
}
