//! Box configuration from the environment the relay injects (sandbox-image.md §4), and the
//! per-workspace view of it. One box per user (design D70): the box holds one workspace per
//! project under `<work_root>/<project>`, each with its own state dir, loopback ports, pi
//! session, subagents, memory DB, and history index.

use std::path::PathBuf;

use serde_json::Value;

fn env_or(k: &str, d: &str) -> String {
    std::env::var(k)
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| d.to_owned())
}

/// Box-level settings: what every workspace shares.
#[derive(Debug, Clone)]
pub struct BoxConfig {
    /// Owner of the box; `None` until `/box/hello` answers (legacy env without it).
    pub user_id: Option<String>,
    pub relay_url: String,
    pub box_token: String,
    pub mcp_token: String,
    /// `/work`: workspaces live in `<work_root>/<project>`; box-level state in `<work_root>/.arbos`.
    pub work_root: PathBuf,
    pub bind: String,
    /// The box HTTP port (health, history, browser stream); the relay's preview URL points here.
    pub port: u16,
    pub local_box: bool,
    pub pi_bin: String,
    pub prime_bin: String,
    pub claude_bin: String,
    pub pi_ext_override: Option<PathBuf>,
    pub device_id: String,
    pub default_talking_model: String,
    pub default_subagent_model: String,
    /// Pre-D70 boxes were created with `ARBOS_PROJECT_ID`; the supervisor treats it as the first
    /// workspace when the relay lists it.
    pub legacy_project_id: Option<String>,
}

impl BoxConfig {
    pub fn from_env() -> anyhow::Result<Self> {
        let work_root = PathBuf::from(env_or("ARBOS_WORK_DIR", "/work"));
        let port: u16 = env_or("ARBOS_PORT", "7788").parse().unwrap_or(7788);
        let sandbox_id = std::env::var("DAYTONA_SANDBOX_ID").ok();
        let local_box = env_or("ARBOS_LOCAL_BOX", "0") == "1";
        let user_id = std::env::var("ARBOS_USER_ID")
            .ok()
            .filter(|s| !s.is_empty());
        let device_id = match (&sandbox_id, &user_id) {
            (Some(id), _) => format!("box:{id}"),
            (None, Some(u)) => format!("box:local-{u}"),
            (None, None) => format!("box:local-{}", env_or("ARBOS_PROJECT_ID", "box")),
        };
        Ok(Self {
            user_id,
            relay_url: env_or("ARBOS_RELAY_URL", "http://127.0.0.1:7700")
                .trim_end_matches('/')
                .to_owned(),
            box_token: env_or("ARBOS_BOX_TOKEN", ""),
            mcp_token: env_or("ARBOS_MCP_TOKEN", ""),
            work_root,
            bind: env_or("ARBOS_BIND", "0.0.0.0"),
            port,
            local_box,
            pi_bin: env_or("ARBOS_PI_BIN", "pi"),
            prime_bin: env_or("ARBOS_PRIME_BIN", "prime-agent"),
            claude_bin: env_or("ARBOS_CLAUDE_BIN", "claude"),
            pi_ext_override: std::env::var("ARBOS_PI_EXT").ok().map(PathBuf::from),
            device_id,
            default_talking_model: env_or("ARBOS_TALKING_MODEL", "anthropic/claude-haiku-5.5"),
            default_subagent_model: env_or("ARBOS_SUBAGENT_MODEL", "anthropic/claude-sonnet-5.5"),
            legacy_project_id: std::env::var("ARBOS_PROJECT_ID")
                .ok()
                .filter(|s| !s.is_empty()),
        })
    }

    /// Box-level state (relay-url file, box logs).
    pub fn box_state_dir(&self) -> PathBuf {
        self.work_root.join(".arbos")
    }

    /// Loopback ports for workspace number `index` (MCP, hook API, model stub). A Daytona box
    /// uses 7800 + 10·index; a local box hangs them off its main port so several boxes share a host.
    fn loopback_base(&self, index: u16) -> u16 {
        if self.local_box {
            self.port + 10 + 10 * index
        } else {
            7800 + 10 * index
        }
    }

    /// The per-workspace config for one project.
    pub fn workspace(&self, project: &Value, index: u16) -> Config {
        let project_id = project
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("project")
            .to_owned();
        let models = project.get("models").cloned().unwrap_or(Value::Null);
        let slot = |k: &str, d: &str| -> String {
            models
                .pointer(&format!("/{k}/id"))
                .and_then(Value::as_str)
                .unwrap_or(d)
                .to_owned()
        };
        let work_dir = self.work_root.join(&project_id);
        let state_dir = work_dir.join(".arbos");
        let base = self.loopback_base(index);
        let talking = slot("talking_model", &self.default_talking_model);
        Config {
            project_id: project_id.clone(),
            project_name: project
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or(&project_id)
                .to_owned(),
            relay_url: self.relay_url.clone(),
            box_token: self.box_token.clone(),
            mcp_token: self.mcp_token.clone(),
            mcp_port: base + 1,
            hook_port: base + 2,
            proxy_port: base + 3,
            decision_model: slot("decision_model", &talking),
            talking_model: talking,
            subagent_model: slot("subagent_model", &self.default_subagent_model),
            instructions: project
                .get("instructions")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned(),
            pi_bin: self.pi_bin.clone(),
            prime_bin: self.prime_bin.clone(),
            claude_bin: self.claude_bin.clone(),
            pi_ext_path: self.pi_ext_override.clone().unwrap_or_else(|| {
                state_dir
                    .join("pi")
                    .join("extensions")
                    .join("arbos-pi-ext")
                    .join("index.ts")
            }),
            device_id: self.device_id.clone(),
            state_dir,
            work_dir,
        }
    }
}

/// One workspace (= one project) as the agent code sees it.
#[derive(Debug, Clone)]
pub struct Config {
    pub project_id: String,
    pub project_name: String,
    pub relay_url: String,
    pub box_token: String,
    pub mcp_token: String,
    pub state_dir: PathBuf,
    pub work_dir: PathBuf,
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
    pub device_id: String,
}

impl Config {
    /// pi provider name: `openrouter` (built-in behaviours over our proxy) unless overridden.
    pub fn pi_provider(&self) -> String {
        env_or("ARBOS_PI_PROVIDER", "openrouter")
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
