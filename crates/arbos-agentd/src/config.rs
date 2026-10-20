//! Box configuration from the environment the relay injects (sandbox-image.md §4), and the
//! per-workspace view of it. One box per user (design D70): the box holds one workspace per
//! project under `<work_root>/<project>`, each with its own state dir, loopback ports, pi
//! session, subagents, memory DB, and history index.

use std::path::PathBuf;

use serde_json::Value;

use crate::isolation::RunAs;
use crate::settings::{DecisionConfig, DecisionProvider};

/// Decision model the group-chat responder decision uses unless the project says otherwise
/// (`ARBOS_DECISION_SCORER`); TypeSafe Jev on OpenRouter's Decisions API.
pub const DEFAULT_DECISION_SCORER: &str = "typesafe/jev-1.14";
/// Yes-probability at or above which an agent replies (`ARBOS_DECISION_THRESHOLD`).
pub const DEFAULT_DECISION_THRESHOLD: f64 = 0.5;

fn env_or(k: &str, d: &str) -> String {
    std::env::var(k)
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| d.to_owned())
}

/// Box-local override of the code-mode flag: `<work_root>/.arbos/code-mode` holding `on` or `off`.
pub const CODE_MODE_FILE: &str = "code-mode";

/// `on` / `off` / `1` / `0` / `true` / `false` / `yes` / `no` → `Some(bool)`; anything else → `None`.
pub fn parse_switch(s: &str) -> Option<bool> {
    match s.trim().to_ascii_lowercase().as_str() {
        "on" | "1" | "true" | "yes" => Some(true),
        "off" | "0" | "false" | "no" => Some(false),
        _ => None,
    }
}

/// Box-local override of the stable-prefix flag: `<work_root>/.arbos/stable-prefix` holding `on` or `off`.
pub const STABLE_PREFIX_FILE: &str = "stable-prefix";

/// A box-wide experiment switch: the box-local `override_file` wins when it holds a valid value (so one
/// test box can run the other arm), else the environment variable `env` (the relay passes its own
/// through, so one relay setting switches the fleet), else `default`.
pub fn switch_on(env: &str, override_file: &std::path::Path, default: bool) -> bool {
    if let Some(v) = std::fs::read_to_string(override_file)
        .ok()
        .and_then(|s| parse_switch(&s))
    {
        return v;
    }
    std::env::var(env)
        .ok()
        .and_then(|s| parse_switch(&s))
        .unwrap_or(default)
}

/// Code mode (D86): the main agent is told to batch and filter tool work in `codemode` scripts, with
/// the script guardrails on. `ARBOS_CODE_MODE=on|off`. Default **off** (the eval showed no win).
pub fn code_mode_on(override_file: &std::path::Path) -> bool {
    switch_on("ARBOS_CODE_MODE", override_file, false)
}

/// Stable prefix (D87): the project-memory block of the system prompt is refreshed at most every
/// 30 min, and the working set / per-turn skills ride inside the user turn (only when they changed)
/// instead of as system-prompt sections, so the cached prefix (tools + system prompt + earlier
/// turns) is reused instead of rewritten. `ARBOS_STABLE_PREFIX=on|off`. Default **on** since the
/// eval of 2026-10-20 (multi-turn chats: −23 % cost, −25 % wall time, 0 prefix rewrites; single-turn
/// chats: tie; quality held).
pub fn stable_prefix_on(override_file: &std::path::Path) -> bool {
    switch_on("ARBOS_STABLE_PREFIX", override_file, true)
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
            browser_stream_port: 9223 + index,
            decision_model: slot("decision_model", &talking),
            decision_cfg: decision_cfg_of(&models),
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
            run_as: None,
        }
    }
}

/// The responder-decision settings of a project's `models.decision_model` slot, with the box
/// environment (`ARBOS_DECISION_PROVIDER` = `jev` | `llm`, `ARBOS_DECISION_SCORER`,
/// `ARBOS_DECISION_THRESHOLD`) as the defaults.
fn decision_cfg_of(models: &Value) -> DecisionConfig {
    let env_provider = DecisionProvider::parse(&env_or("ARBOS_DECISION_PROVIDER", "jev"))
        .unwrap_or(DecisionProvider::Jev);
    let env_threshold = env_or("ARBOS_DECISION_THRESHOLD", "")
        .parse::<f64>()
        .ok()
        .filter(|t| (0.0..=1.0).contains(t))
        .unwrap_or(DEFAULT_DECISION_THRESHOLD);
    DecisionConfig {
        provider: models
            .pointer("/decision_model/provider")
            .and_then(Value::as_str)
            .and_then(DecisionProvider::parse)
            .unwrap_or(env_provider),
        scorer: models
            .pointer("/decision_model/scorer")
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| env_or("ARBOS_DECISION_SCORER", DEFAULT_DECISION_SCORER)),
        threshold: models
            .pointer("/decision_model/threshold")
            .and_then(Value::as_f64)
            .filter(|t| (0.0..=1.0).contains(t))
            .unwrap_or(env_threshold),
        memory_tree: models
            .pointer("/decision_model/memory_tree")
            .and_then(Value::as_str)
            .and_then(crate::tree::TreeMode::parse)
            .unwrap_or_else(|| {
                crate::tree::TreeMode::parse(&env_or("ARBOS_MEMORY_TREE", "on"))
                    .unwrap_or(crate::tree::TreeMode::On)
            }),
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
    /// agent-browser's JPEG stream port for this workspace (9223 + index); one browser per workspace.
    pub browser_stream_port: u16,
    pub talking_model: String,
    pub subagent_model: String,
    pub decision_model: String,
    /// Responder decision: provider switch, decision-model id, threshold (`decision_model` slot).
    pub decision_cfg: DecisionConfig,
    pub instructions: String,
    pub pi_bin: String,
    pub prime_bin: String,
    pub claude_bin: String,
    pub pi_ext_path: PathBuf,
    pub device_id: String,
    /// The Unix identity this workspace's children run as (`None` = agentd's own, local boxes).
    pub run_as: Option<RunAs>,
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

    /// The bearer token subagent sessions present to the MCP server and hooks. Derived from
    /// the main token so the main session's token never reaches a subagent process; the MCP
    /// server tells the two apart to refuse main-only tools (D69: subagents never talk
    /// project-to-project).
    pub fn subagent_mcp_token(&self) -> String {
        use sha2::Digest;
        let digest = sha2::Sha256::digest(format!("subagent:{}", self.mcp_token).as_bytes());
        format!("sub_{}", hex::encode(&digest[..16]))
    }

    /// The MCP token a session of `role` (`main` or `subagent`) is given.
    pub fn mcp_token_for_role(&self, role: &str) -> String {
        if role == "main" {
            self.mcp_token.clone()
        } else {
            self.subagent_mcp_token()
        }
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

    /// Code mode for this workspace's main agent (D86). Read at every pi (re)spawn, so a toggle
    /// needs a pi respawn, not a box restart.
    pub fn code_mode(&self) -> bool {
        code_mode_on(&self.box_flag_file(CODE_MODE_FILE))
    }

    /// Stable prefix for this workspace's main agent (D87); read at every pi (re)spawn.
    pub fn stable_prefix(&self) -> bool {
        stable_prefix_on(&self.box_flag_file(STABLE_PREFIX_FILE))
    }

    /// `<work_root>/.arbos/<name>`: a box-wide flag file an operator can set for one box.
    fn box_flag_file(&self, name: &str) -> PathBuf {
        let root = self.work_dir.parent().unwrap_or(&self.work_dir);
        root.join(".arbos").join(name)
    }

    /// Browser settings per workspace: own profile, screenshots, stream port (the image's
    /// box-wide defaults would collide between workspaces and are unwritable to their users).
    pub fn browser_env(&self) -> Vec<(String, String)> {
        vec![
            (
                "AGENT_BROWSER_PROFILE".into(),
                self.state_dir.join("browser").display().to_string(),
            ),
            (
                "AGENT_BROWSER_SCREENSHOT_DIR".into(),
                self.state_dir.join("shots").display().to_string(),
            ),
            (
                "AGENT_BROWSER_STREAM_PORT".into(),
                self.browser_stream_port.to_string(),
            ),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn switch_words() {
        assert_eq!(parse_switch(" ON\n"), Some(true));
        assert_eq!(parse_switch("off"), Some(false));
        assert_eq!(parse_switch("1"), Some(true));
        assert_eq!(parse_switch("maybe"), None);
        assert_eq!(parse_switch(""), None);
    }

    /// The box-local file wins over the environment; a garbage file falls back to the env; the
    /// default is off.
    #[test]
    fn code_mode_file_overrides_env() {
        let dir = std::env::temp_dir().join(format!("arbos-cm-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join(CODE_MODE_FILE);
        let _ = std::fs::remove_file(&f);
        // No env in the test process unless the runner set one: the default is off.
        if std::env::var("ARBOS_CODE_MODE").is_err() {
            assert!(!code_mode_on(&f));
        }
        std::fs::write(&f, "on\n").unwrap();
        assert!(code_mode_on(&f));
        std::fs::write(&f, "off").unwrap();
        assert!(!code_mode_on(&f));
        std::fs::write(&f, "banana").unwrap();
        if std::env::var("ARBOS_CODE_MODE").is_err() {
            assert!(!code_mode_on(&f));
        }
        let sp = dir.join(STABLE_PREFIX_FILE);
        if std::env::var("ARBOS_STABLE_PREFIX").is_err() {
            assert!(stable_prefix_on(&sp), "stable prefix is on by default");
        }
        std::fs::write(&sp, "off").unwrap();
        assert!(!stable_prefix_on(&sp), "the box file turns it off");
        std::fs::write(&sp, "on").unwrap();
        assert!(stable_prefix_on(&sp));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
