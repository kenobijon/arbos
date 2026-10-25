//! Draft-and-approve (design §10, S18): the request/outcome types every feature shares, the
//! `approval` doc-part shape, and the risky-action list as data with one matcher. The box is still
//! the safety boundary (D16); this list only decides which actions get a card before they run.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Pending cards expire after this long (`expiresAt`).
pub const APPROVAL_TTL_MS: i64 = 24 * 60 * 60 * 1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Risk {
    /// Outgoing communication: email, chat post, calendar invite, GitHub comment/issue/PR, social post.
    Send,
    Destructive,
    Money,
    Credential,
    /// Any command or file read on the user's own computer.
    Computer,
    Other,
}

impl Risk {
    pub fn as_str(self) -> &'static str {
        match self {
            Risk::Send => "send",
            Risk::Destructive => "destructive",
            Risk::Money => "money",
            Risk::Credential => "credential",
            Risk::Computer => "computer",
            Risk::Other => "other",
        }
    }

    pub fn parse(s: &str) -> Risk {
        match s {
            "send" => Risk::Send,
            "destructive" => Risk::Destructive,
            "money" => Risk::Money,
            "credential" => Risk::Credential,
            "computer" => Risk::Computer,
            _ => Risk::Other,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalState {
    Pending,
    Approved,
    Rejected,
    Expired,
}

impl ApprovalState {
    pub fn as_str(self) -> &'static str {
        match self {
            ApprovalState::Pending => "pending",
            ApprovalState::Approved => "approved",
            ApprovalState::Rejected => "rejected",
            ApprovalState::Expired => "expired",
        }
    }
}

/// What the agent wants to do. `payload` is rendered by the card (a draft shows `to`,
/// `subject`, `body`; a command shows `command`) and is handed back to the caller on approval.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApprovalRequest {
    pub title: String,
    pub summary: String,
    pub risk: Risk,
    #[serde(default)]
    pub payload: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ApprovalOutcome {
    Approved,
    Rejected {
        #[serde(default)]
        reason: Option<String>,
    },
    Expired,
}

impl ApprovalOutcome {
    pub fn state(&self) -> ApprovalState {
        match self {
            ApprovalOutcome::Approved => ApprovalState::Approved,
            ApprovalOutcome::Rejected { .. } => ApprovalState::Rejected,
            ApprovalOutcome::Expired => ApprovalState::Expired,
        }
    }

    pub fn is_approved(&self) -> bool {
        matches!(self, ApprovalOutcome::Approved)
    }
}

/// The dispatch rule every gate shares: a risky action runs only after an `Approved` outcome.
/// `None` (no card was ever decided) is a refusal, never a pass.
pub fn may_dispatch(outcome: Option<&ApprovalOutcome>) -> bool {
    matches!(outcome, Some(ApprovalOutcome::Approved))
}

/// Which part of a tool call a rule inspects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Subject {
    /// The tool name alone.
    ToolName,
    /// The connector action name (`args.action`) or the explicit `args.kind`.
    ConnectorAction,
    /// The shell command text of a `bash`-like tool.
    ShellCommand,
}

/// One entry of the risky-action list.
#[derive(Debug, Clone, Copy)]
pub struct RiskyRule {
    pub class: &'static str,
    pub risk: Risk,
    pub subject: Subject,
    /// Regex tested against the subject (case-insensitive).
    pub pattern: &'static str,
    pub why: &'static str,
}

/// Tools whose names look like sends but are box-internal or cross-project (D22): never gated.
/// `send_message` is agent-to-agent (or an in-group speak): the owner's agents may message each
/// other without a card. Risky work the *receiving* agent then does still raises a card in that
/// agent's chat. External sends stay gated via connector / draft tools.
pub const NOT_RISKY_TOOLS: &[&str] = &[
    "projects_send",
    "group_send",
    "send_message",
    "draft_message",
    "ask_user",
    "route",
    "remember",
    "recall",
    "peek",
    "deliver_file",
    "routines_list",
    "routines_create",
    "routines_update",
    "routines_delete",
];

/// Shell tools whose `command` argument is checked against the destructive patterns.
pub const SHELL_TOOLS: &[&str] = &["bash", "shell", "run_command", "execute_command"];

/// Connector actions of kind `send`/`write` when the caller passes no explicit `kind`.
const CONNECTOR_SEND_ACTIONS: &str = r"^(send|send_\w+|\w+_send|post|post_message|reply|comment|create_event|create_issue|create_pr|create_pull_request|publish|tweet|invite|share|forward)$";

pub const RISKY_RULES: &[RiskyRule] = &[
    // ---- outgoing communications --------------------------------------------------------------
    RiskyRule {
        class: "connector-send",
        risk: Risk::Send,
        subject: Subject::ConnectorAction,
        pattern: CONNECTOR_SEND_ACTIONS,
        why: "leaves the box as a message the user did not write",
    },
    RiskyRule {
        class: "connector-write",
        risk: Risk::Send,
        subject: Subject::ConnectorAction,
        pattern: r"^(write|send|delete|trash|archive|modify|update|move|label)$",
        why: "writes into a user account",
    },
    RiskyRule {
        class: "tool-send",
        risk: Risk::Send,
        subject: Subject::ToolName,
        pattern: r"(^|_)(send|post|publish|tweet|invite|reply)(_|$)",
        why: "outgoing communication",
    },
    // ---- money -----------------------------------------------------------------------------------
    RiskyRule {
        class: "money",
        risk: Risk::Money,
        subject: Subject::ToolName,
        pattern: r"(^|_)(pay|payment|purchase|checkout|transfer|refund|charge|buy|order|wire)(_|$)",
        why: "moves money",
    },
    RiskyRule {
        class: "connector-money",
        risk: Risk::Money,
        subject: Subject::ConnectorAction,
        pattern: r"(^|_)(pay|payment|purchase|checkout|transfer|refund|charge|buy|order|wire)(_|$)",
        why: "moves money",
    },
    // ---- credentials ------------------------------------------------------------------------------
    RiskyRule {
        class: "vault-request",
        risk: Risk::Credential,
        subject: Subject::ToolName,
        pattern: r"^(vault_request|vault_use|browser_fill)$",
        why: "uses a stored credential",
    },
    RiskyRule {
        class: "oauth-grant",
        risk: Risk::Credential,
        subject: Subject::ToolName,
        pattern: r"(oauth|_grant|grant_|token_export|export_token|connector_connect)",
        why: "grants or exports a credential",
    },
    // ---- user computer ----------------------------------------------------------------------------
    RiskyRule {
        class: "local-computer",
        risk: Risk::Computer,
        subject: Subject::ToolName,
        pattern: r"^local_(run|read|list|exec|write)$",
        why: "touches the user's own computer",
    },
    // ---- destructive shell --------------------------------------------------------------------------
    RiskyRule {
        class: "rm-recursive",
        risk: Risk::Destructive,
        subject: Subject::ShellCommand,
        pattern: r"(^|[;&|]\s*|\s)rm\s+(-\w*[rRf]\w*\s+)+",
        why: "recursive or forced delete outside /work/tmp",
    },
    RiskyRule {
        class: "git-force-push",
        risk: Risk::Destructive,
        subject: Subject::ShellCommand,
        pattern: r"git\s+push\b[^;&|]*(\s--force(-with-lease)?\b|\s-f\b|\s\+\S+)",
        why: "rewrites remote history",
    },
    RiskyRule {
        class: "git-branch-delete",
        risk: Risk::Destructive,
        subject: Subject::ShellCommand,
        pattern: r"git\s+(branch\s+(-D|-d|--delete)\b|push\b[^;&|]*(\s--delete\b|\s:\S+))",
        why: "deletes a branch",
    },
    RiskyRule {
        class: "sql-drop",
        risk: Risk::Destructive,
        subject: Subject::ShellCommand,
        pattern: r"\b(DROP\s+(TABLE|DATABASE|SCHEMA|INDEX|VIEW)|TRUNCATE(\s+TABLE)?)\b",
        why: "drops or truncates data",
    },
    RiskyRule {
        class: "cloud-delete",
        risk: Risk::Destructive,
        subject: Subject::ShellCommand,
        pattern: r"\b(aws|gcloud|az|doctl|flyctl|fly|heroku|vercel|terraform|pulumi|daytona|kubectl|helm)\s+[^;&|]*\b(delete|destroy|rm|remove|terminate|purge)\b",
        why: "deletes a cloud resource",
    },
    RiskyRule {
        class: "repo-delete",
        risk: Risk::Destructive,
        subject: Subject::ShellCommand,
        pattern: r"\bgh\s+(repo|release|gist)\s+delete\b",
        why: "deletes something in a user account",
    },
    RiskyRule {
        class: "container-prune",
        risk: Risk::Destructive,
        subject: Subject::ShellCommand,
        pattern: r"\bdocker\s+(system\s+prune|volume\s+(rm|prune)|rmi?)\b",
        why: "removes containers, images or volumes",
    },
    RiskyRule {
        class: "disk-wipe",
        risk: Risk::Destructive,
        subject: Subject::ShellCommand,
        pattern: r"(\bmkfs\b|\bdd\s+[^;&|]*of=/dev/|>\s*/dev/(sd|nvme|disk)|\bshred\b|\bwipefs\b)",
        why: "wipes a disk",
    },
];

/// Why a call needs a card. `path_class` is set for shell rules whose targets are paths
/// (`rm-recursive`): the class a standing grant must cover to skip the card.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RiskMatch {
    pub class: &'static str,
    pub risk: Risk,
    pub why: &'static str,
    pub path_class: Option<PathClass>,
}

/// Where a shell command's filesystem targets live, widest last. A standing grant for a class
/// covers every narrower class (#1544: "always allow in /tmp" never covers the workspace).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PathClass {
    /// `/tmp` and `/work/tmp`: the box scratch area.
    Scratch,
    /// The agent's own workspace (`/work`).
    Workspace,
    /// Anything else on the box.
    Anywhere,
}

/// The agent's workspace in the box.
pub const WORKSPACE_ROOT: &str = "/work";
/// Scratch roots: deletes under these never raise a card.
pub const SCRATCH_ROOTS: &[&str] = &["/tmp", "/work/tmp"];

impl PathClass {
    pub fn as_str(self) -> &'static str {
        match self {
            PathClass::Scratch => "scratch",
            PathClass::Workspace => "workspace",
            PathClass::Anywhere => "anywhere",
        }
    }

    pub fn parse(s: &str) -> Option<PathClass> {
        match s {
            "scratch" => Some(PathClass::Scratch),
            "workspace" => Some(PathClass::Workspace),
            "anywhere" => Some(PathClass::Anywhere),
            _ => None,
        }
    }

    /// Short label for the card option and the Settings row ("Always allow Doug in /tmp").
    pub fn label(self) -> &'static str {
        match self {
            PathClass::Scratch => "/tmp",
            PathClass::Workspace => "its workspace",
            PathClass::Anywhere => "anywhere",
        }
    }

    /// One plain line for the option hint / Settings subtitle.
    pub fn describe(self) -> &'static str {
        match self {
            PathClass::Scratch => "scratch only: /tmp and /work/tmp",
            PathClass::Workspace => "the agent's own /work folder, including scratch",
            PathClass::Anywhere => "any folder of its box",
        }
    }

    /// A grant for `self` lets a command whose targets fall in `other` run without a card.
    pub fn covers(self, other: PathClass) -> bool {
        self >= other
    }
}

/// One standing shell grant as the relay stores it (`shell_grants`): "<agent> may run
/// `rule_class` commands whose targets are within `path_class` without a card", optionally
/// only in one chat. Revoked rows are not listed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShellGrant {
    pub id: String,
    pub project_id: String,
    pub rule_class: String,
    pub path_class: PathClass,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat_id: Option<String>,
    pub granted_by: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_id: Option<String>,
    pub created_at: i64,
}

impl ShellGrant {
    /// Whether this grant lets `hit` (raised in `chat_id`) run without a card.
    pub fn covers(&self, hit: &RiskMatch, chat_id: Option<&str>) -> bool {
        let Some(class) = hit.path_class else {
            return false;
        };
        self.rule_class == hit.class
            && self.path_class.covers(class)
            && self.chat_id.as_deref().is_none_or(|c| Some(c) == chat_id)
    }
}

/// Rule classes a standing grant may cover: shell rules whose risk is in filesystem paths.
pub const GRANTABLE_CLASSES: &[&str] = &["rm-recursive"];

fn regex(pattern: &str) -> Option<regex_lite::Regex> {
    regex_lite::RegexBuilder::new(pattern)
        .case_insensitive(true)
        .build()
        .ok()
}

/// `cwd` + `target` as a normalized absolute path (`.`/`..` folded). `~` and `$HOME` resolve
/// nowhere we classify, so they come back as-is and classify as `Anywhere`.
fn resolve_path(cwd: &str, target: &str) -> String {
    let t = target.trim_matches(|c| c == '"' || c == '\'');
    if t.starts_with('~') || t.starts_with('$') {
        return t.to_owned();
    }
    let joined = if t.starts_with('/') {
        t.to_owned()
    } else if cwd.starts_with('/') {
        format!("{cwd}/{t}")
    } else {
        return format!("{cwd}/{t}");
    };
    let mut out: Vec<&str> = Vec::new();
    for seg in joined.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                out.pop();
            }
            s => out.push(s),
        }
    }
    format!("/{}", out.join("/"))
}

fn under(path: &str, root: &str) -> bool {
    path == root || path.starts_with(&format!("{root}/"))
}

/// Class of one absolute path. A glob keeps the class of its fixed prefix.
pub fn classify_path(path: &str) -> PathClass {
    if !path.starts_with('/') {
        return PathClass::Anywhere;
    }
    if SCRATCH_ROOTS.iter().any(|r| under(path, r)) {
        return PathClass::Scratch;
    }
    if under(path, WORKSPACE_ROOT) {
        return PathClass::Workspace;
    }
    PathClass::Anywhere
}

/// Split a command line into simple commands at `;`, `&&`, `||`, `|` and newlines, keeping
/// order so a leading `cd` applies to what follows.
fn simple_commands(command: &str) -> Vec<Vec<String>> {
    let mut cmds = Vec::new();
    let mut cur: Vec<String> = Vec::new();
    let mut word = String::new();
    let mut quote: Option<char> = None;
    let mut chars = command.chars().peekable();
    let flush_word = |word: &mut String, cur: &mut Vec<String>| {
        if !word.is_empty() {
            cur.push(std::mem::take(word));
        }
    };
    while let Some(c) = chars.next() {
        match quote {
            Some(q) => {
                if c == q {
                    quote = None;
                } else {
                    word.push(c);
                }
            }
            None => match c {
                '\'' | '"' => quote = Some(c),
                ';' | '\n' | '|' | '&' => {
                    if (c == '|' || c == '&') && chars.peek() == Some(&c) {
                        chars.next();
                    }
                    flush_word(&mut word, &mut cur);
                    if !cur.is_empty() {
                        cmds.push(std::mem::take(&mut cur));
                    }
                }
                c if c.is_whitespace() => flush_word(&mut word, &mut cur),
                c => word.push(c),
            },
        }
    }
    flush_word(&mut word, &mut cur);
    if !cur.is_empty() {
        cmds.push(cur);
    }
    cmds
}

fn is_recursive_flag(w: &str) -> bool {
    w == "--recursive"
        || (w.starts_with('-') && !w.starts_with("--") && w[1..].contains(['r', 'R']))
}

/// The widest class among the targets of every `rm` in `command` (a leading `cd` moves the
/// working directory; the default is the workspace), and whether any of them is recursive.
/// `None` when the command deletes nothing.
pub fn rm_targets(command: &str) -> Option<(PathClass, bool)> {
    let mut cwd = WORKSPACE_ROOT.to_owned();
    let mut widest: Option<PathClass> = None;
    let mut recursive = false;
    for cmd in simple_commands(command) {
        let mut words = cmd.iter().map(String::as_str).peekable();
        let mut head = words.next()?;
        while matches!(head, "sudo" | "env" | "time" | "nice" | "nohup") {
            head = words.next()?;
        }
        match head {
            "cd" => {
                cwd = match words.find(|w| !w.starts_with('-')) {
                    Some(dir) => resolve_path(&cwd, dir),
                    None => "~".to_owned(),
                };
            }
            "pushd" => {
                if let Some(dir) = words.find(|w| !w.starts_with('-')) {
                    cwd = resolve_path(&cwd, dir);
                }
            }
            "rm" => {
                let mut saw_dashdash = false;
                for w in words {
                    if !saw_dashdash && w == "--" {
                        saw_dashdash = true;
                        continue;
                    }
                    if !saw_dashdash && w.starts_with('-') {
                        recursive |= is_recursive_flag(w);
                        continue;
                    }
                    let class = classify_path(&resolve_path(&cwd, w));
                    widest = Some(widest.map_or(class, |c| c.max(class)));
                }
            }
            _ => {}
        }
    }
    widest.map(|c| (c, recursive))
}

/// Flag a shell command when it matches a destructive pattern. An `rm` whose targets all sit in
/// scratch is not destructive; a non-recursive `rm -f` of files in the agent's own workspace is
/// an edit of its working tree, not a card (#1544).
pub fn classify_shell(command: &str) -> Option<RiskMatch> {
    for rule in RISKY_RULES {
        if rule.subject != Subject::ShellCommand {
            continue;
        }
        let Some(re) = regex(rule.pattern) else {
            continue;
        };
        if !re.is_match(command) {
            continue;
        }
        let mut path_class = None;
        if rule.class == "rm-recursive" {
            match rm_targets(command) {
                Some((PathClass::Scratch, _)) => continue,
                Some((PathClass::Workspace, false)) => continue,
                Some((class, _)) => path_class = Some(class),
                None => path_class = Some(PathClass::Anywhere),
            }
        }
        return Some(RiskMatch {
            class: rule.class,
            risk: rule.risk,
            why: rule.why,
            path_class,
        });
    }
    None
}

fn connector_subject(args: &Value) -> Option<String> {
    if let Some(kind) = args.get("kind").and_then(Value::as_str) {
        if kind == "send" || kind == "write" {
            return Some(kind.to_owned());
        }
        if kind == "read" {
            return None;
        }
    }
    args.get("action")
        .and_then(Value::as_str)
        .map(str::to_owned)
}

/// Flag a tool call (name + JSON args) that is on the risky list. Reads, searches, drafts and
/// box-local edits return `None`.
pub fn classify_tool(tool: &str, args: &Value) -> Option<RiskMatch> {
    if NOT_RISKY_TOOLS.contains(&tool) {
        return None;
    }
    if SHELL_TOOLS.contains(&tool) {
        let cmd = args
            .get("command")
            .or_else(|| args.get("cmd"))
            .and_then(Value::as_str)
            .unwrap_or("");
        return classify_shell(cmd);
    }
    let is_connector = tool == "connector_call";
    let connector_subject = if is_connector {
        connector_subject(args)
    } else {
        None
    };
    for rule in RISKY_RULES {
        let hit = match rule.subject {
            Subject::ToolName => regex(rule.pattern).is_some_and(|re| re.is_match(tool)),
            Subject::ConnectorAction => connector_subject
                .as_deref()
                .is_some_and(|s| regex(rule.pattern).is_some_and(|re| re.is_match(s))),
            Subject::ShellCommand => false,
        };
        if hit {
            return Some(RiskMatch {
                class: rule.class,
                risk: rule.risk,
                why: rule.why,
                path_class: None,
            });
        }
    }
    None
}

/// Card title + summary for a gated tool call.
pub fn describe_tool(tool: &str, args: &Value, hit: &RiskMatch) -> (String, String) {
    let one_line = |v: &Value| -> String {
        let s = match v {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        let s: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
        if s.chars().count() > 160 {
            format!("{}…", s.chars().take(159).collect::<String>())
        } else {
            s
        }
    };
    if SHELL_TOOLS.contains(&tool) {
        let cmd = args
            .get("command")
            .or_else(|| args.get("cmd"))
            .cloned()
            .unwrap_or(Value::Null);
        return (
            format!("Run a destructive command ({})", hit.class),
            one_line(&cmd),
        );
    }
    if tool == "connector_call" {
        let provider = args.get("provider").and_then(Value::as_str).unwrap_or("?");
        let action = args.get("action").and_then(Value::as_str).unwrap_or("?");
        return (
            format!("{provider}: {action}"),
            one_line(args.get("args").unwrap_or(&Value::Null)),
        );
    }
    (format!("{tool} ({})", hit.class), one_line(args))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Invariant: the matcher flags every listed class and leaves plain reads alone.
    #[test]
    fn matcher_flags_each_risky_class() {
        let send = classify_tool(
            "connector_call",
            &json!({ "provider": "google", "action": "send", "args": {} }),
        )
        .expect("email send");
        assert_eq!(send.risk, Risk::Send);
        assert_eq!(
            classify_tool(
                "connector_call",
                &json!({ "provider": "slack", "action": "post_message" })
            )
            .map(|m| m.risk),
            Some(Risk::Send)
        );
        assert_eq!(
            classify_tool(
                "connector_call",
                &json!({ "provider": "google", "action": "create_event" })
            )
            .map(|m| m.risk),
            Some(Risk::Send)
        );
        assert_eq!(
            classify_tool(
                "connector_call",
                &json!({ "provider": "github", "action": "comment" })
            )
            .map(|m| m.risk),
            Some(Risk::Send)
        );
        assert_eq!(
            classify_tool(
                "connector_call",
                &json!({ "provider": "github", "action": "create_issue", "kind": "write" })
            )
            .map(|m| m.risk),
            Some(Risk::Send)
        );
        assert_eq!(
            classify_tool("social_post", &json!({})).map(|m| m.risk),
            Some(Risk::Send)
        );
        assert_eq!(
            classify_tool("stripe_payment", &json!({})).map(|m| m.risk),
            Some(Risk::Money)
        );
        assert_eq!(
            classify_tool(
                "connector_call",
                &json!({ "provider": "bank", "action": "transfer" })
            )
            .map(|m| m.risk),
            Some(Risk::Money)
        );
        assert_eq!(
            classify_tool("vault_request", &json!({ "name": "github" })).map(|m| m.risk),
            Some(Risk::Credential)
        );
        assert_eq!(
            classify_tool("oauth_grant", &json!({})).map(|m| m.risk),
            Some(Risk::Credential)
        );
        assert_eq!(
            classify_tool("local_run", &json!({ "cmd": "uname -a" })).map(|m| m.risk),
            Some(Risk::Computer)
        );
        assert_eq!(
            classify_tool("local_read", &json!({ "path": "~/notes.md" })).map(|m| m.risk),
            Some(Risk::Computer)
        );

        for cmd in [
            "rm -rf /work/project/src",
            "cd /work && rm -r build",
            "git push --force origin main",
            "git push -f",
            "git push origin +main",
            "git branch -D feature",
            "git push origin --delete feature",
            "psql -c 'DROP TABLE users'",
            "sqlite3 db.sqlite 'TRUNCATE TABLE logs'",
            "aws s3 rb s3://bucket --force && aws ec2 terminate-instances --instance-ids i-1",
            "gcloud compute instances delete vm-1",
            "kubectl delete deployment web",
            "gh repo delete unarbos/arbos --yes",
            "docker system prune -af",
            "dd if=/dev/zero of=/dev/sda",
        ] {
            let hit = classify_tool("bash", &json!({ "command": cmd }));
            assert_eq!(
                hit.as_ref().map(|m| m.risk),
                Some(Risk::Destructive),
                "{cmd} should be destructive"
            );
        }
    }

    #[test]
    fn matcher_ignores_reads_drafts_and_scratch() {
        for (tool, args) in [
            ("bash", json!({ "command": "ls -la" })),
            ("bash", json!({ "command": "cat README.md | head" })),
            ("bash", json!({ "command": "rm -rf /work/tmp/build" })),
            ("bash", json!({ "command": "rm -rf tmp/cache" })),
            ("bash", json!({ "command": "git push origin main" })),
            ("bash", json!({ "command": "cargo test --workspace" })),
            ("read", json!({ "path": "src/main.rs" })),
            ("edit", json!({ "path": "src/main.rs" })),
            ("write", json!({ "path": "/work/out/report.md" })),
            ("history_search", json!({ "query": "rm -rf" })),
            (
                "connector_call",
                json!({ "provider": "google", "action": "read_message" }),
            ),
            (
                "connector_call",
                json!({ "provider": "google", "action": "search", "kind": "read" }),
            ),
            (
                "connector_call",
                json!({ "provider": "google", "action": "create_draft" }),
            ),
            (
                "draft_message",
                json!({ "channel": "email", "to": "a@b.c" }),
            ),
            ("projects_send", json!({ "to": "Comms", "text": "hi" })),
            ("send_message", json!({ "to": "TestAgent", "text": "hi" })),
            ("send_message", json!({ "text": "still working" })),
            ("local_devices", json!({})),
        ] {
            assert_eq!(
                classify_tool(tool, &args),
                None,
                "{tool} {args} is not risky"
            );
        }
    }

    /// #1544: a leading `cd` into scratch makes relative targets scratch; a non-recursive `rm -f`
    /// inside the workspace is not a card; the class names where the targets are.
    #[test]
    fn rm_classes_follow_cd_and_target_paths() {
        for cmd in [
            "cd /tmp && rm -rf arbos-src && git clone --depth 1 https://x/y arbos-src",
            "cd /work/tmp; rm -r build",
            "pushd /tmp && rm -rf ./cache",
            "rm -f out/*.png",
            "rm -f /work/out/a.png /work/out/b.png",
            "rm -- -weird-file",
        ] {
            assert_eq!(classify_shell(cmd), None, "{cmd} needs no card");
        }
        let ws = classify_shell("rm -rf build").expect("workspace recursive delete cards");
        assert_eq!(ws.path_class, Some(PathClass::Workspace));
        let ws2 = classify_shell("cd /work/app && rm -rf node_modules").expect("cards");
        assert_eq!(ws2.path_class, Some(PathClass::Workspace));
        let any = classify_shell("cd /tmp && rm -rf ../etc/foo").expect("escapes scratch");
        assert_eq!(any.path_class, Some(PathClass::Anywhere));
        let any2 = classify_shell("rm -rf ~/Library").expect("home cards");
        assert_eq!(any2.path_class, Some(PathClass::Anywhere));
        let mixed = classify_shell("rm -rf /tmp/a /work/b").expect("widest target wins");
        assert_eq!(mixed.path_class, Some(PathClass::Workspace));
        let plain_f = classify_shell("rm -f /etc/hosts").expect("forced delete outside /work");
        assert_eq!(plain_f.path_class, Some(PathClass::Anywhere));
        assert_eq!(
            classify_shell("git push --force origin main")
                .expect("force push")
                .path_class,
            None
        );
    }

    /// A standing grant covers its class and every narrower one, for its chat or all chats.
    #[test]
    fn shell_grant_covers_narrower_classes_only() {
        let grant = |class: PathClass, chat: Option<&str>| ShellGrant {
            id: "g".into(),
            project_id: "p".into(),
            rule_class: "rm-recursive".into(),
            path_class: class,
            chat_id: chat.map(str::to_owned),
            granted_by: "u".into(),
            approval_id: None,
            created_at: 0,
        };
        let ws = classify_shell("rm -rf build").unwrap();
        let any = classify_shell("rm -rf /etc/x").unwrap();
        assert!(grant(PathClass::Workspace, None).covers(&ws, Some("c1")));
        assert!(!grant(PathClass::Workspace, None).covers(&any, Some("c1")));
        assert!(grant(PathClass::Anywhere, None).covers(&any, None));
        assert!(!grant(PathClass::Scratch, None).covers(&ws, None));
        assert!(grant(PathClass::Anywhere, Some("c1")).covers(&ws, Some("c1")));
        assert!(!grant(PathClass::Anywhere, Some("c1")).covers(&ws, Some("c2")));
        let push = classify_shell("git push -f").unwrap();
        assert!(!grant(PathClass::Anywhere, None).covers(&push, None));
        assert!(PathClass::Anywhere.covers(PathClass::Scratch));
        assert!(!PathClass::Scratch.covers(PathClass::Workspace));
        assert_eq!(PathClass::parse("workspace"), Some(PathClass::Workspace));
    }

    /// Invariant: a `send`-kind action with no `approved` outcome is never dispatched.
    #[test]
    fn send_without_approval_never_dispatches() {
        assert!(!may_dispatch(None));
        assert!(!may_dispatch(Some(&ApprovalOutcome::Rejected {
            reason: Some("no".into())
        })));
        assert!(!may_dispatch(Some(&ApprovalOutcome::Expired)));
        assert!(may_dispatch(Some(&ApprovalOutcome::Approved)));
    }
}
