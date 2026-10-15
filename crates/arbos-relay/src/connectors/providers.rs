//! Provider catalog (actions as data), OAuth settings, upstream request shapes, and the
//! canned replies used when `ARBOS_CONNECTORS_MOCK=1`.

use base64::engine::general_purpose::{STANDARD as B64, URL_SAFE_NO_PAD as B64URL};
use base64::Engine;
use reqwest::Method;
use serde_json::{json, Value};

use arbos_core::ids::now_ms;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Provider {
    Google,
    Slack,
    Github,
    HarnessComms,
}

impl Provider {
    pub const ALL: [Provider; 4] = [
        Provider::Google,
        Provider::Slack,
        Provider::Github,
        Provider::HarnessComms,
    ];

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "google" => Some(Provider::Google),
            "slack" => Some(Provider::Slack),
            "github" => Some(Provider::Github),
            "harness_comms" => Some(Provider::HarnessComms),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Provider::Google => "google",
            Provider::Slack => "slack",
            Provider::Github => "github",
            Provider::HarnessComms => "harness_comms",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Provider::Google => "Google (Gmail, Calendar)",
            Provider::Slack => "Slack",
            Provider::Github => "GitHub",
            Provider::HarnessComms => "Harness Comms (Beeper + Gmail)",
        }
    }

    /// How the user connects: an OAuth 2 code flow, a pasted token, or either.
    pub fn auth(self) -> Auth {
        match self {
            Provider::Google => Auth::Oauth(OauthConfig {
                auth_url: "https://accounts.google.com/o/oauth2/v2/auth",
                token_url: "https://oauth2.googleapis.com/token",
                scopes: &[
                    "https://www.googleapis.com/auth/gmail.readonly",
                    "https://www.googleapis.com/auth/gmail.compose",
                    "https://www.googleapis.com/auth/gmail.send",
                    "https://www.googleapis.com/auth/calendar.events",
                ],
                client_id_env: "ARBOS_GOOGLE_CLIENT_ID",
                client_secret_env: "ARBOS_GOOGLE_CLIENT_SECRET",
                extra_auth_params: &[("access_type", "offline"), ("prompt", "consent")],
                accepts_token: false,
            }),
            Provider::Slack => Auth::Oauth(OauthConfig {
                auth_url: "https://slack.com/oauth/v2/authorize",
                token_url: "https://slack.com/api/oauth.v2.access",
                scopes: &[
                    "channels:read",
                    "channels:history",
                    "groups:read",
                    "groups:history",
                    "search:read",
                    "chat:write",
                ],
                client_id_env: "ARBOS_SLACK_CLIENT_ID",
                client_secret_env: "ARBOS_SLACK_CLIENT_SECRET",
                extra_auth_params: &[],
                accepts_token: true,
            }),
            Provider::Github => Auth::Oauth(OauthConfig {
                auth_url: "https://github.com/login/oauth/authorize",
                token_url: "https://github.com/login/oauth/access_token",
                scopes: &["repo", "read:user"],
                client_id_env: "ARBOS_GITHUB_CLIENT_ID",
                client_secret_env: "ARBOS_GITHUB_CLIENT_SECRET",
                extra_auth_params: &[],
                accepts_token: true,
            }),
            Provider::HarnessComms => Auth::Token,
        }
    }

    pub fn actions(self) -> &'static [ActionSpec] {
        match self {
            Provider::Google => GOOGLE_ACTIONS,
            Provider::Slack => SLACK_ACTIONS,
            Provider::Github => GITHUB_ACTIONS,
            Provider::HarnessComms => HARNESS_ACTIONS,
        }
    }

    pub fn action(self, name: &str) -> Option<&'static ActionSpec> {
        self.actions().iter().find(|a| a.name == name)
    }
}

pub enum Auth {
    Oauth(OauthConfig),
    /// API key pasted by the user (`POST /connectors/{provider}/connect { token }`).
    Token,
}

pub struct OauthConfig {
    pub auth_url: &'static str,
    pub token_url: &'static str,
    pub scopes: &'static [&'static str],
    pub client_id_env: &'static str,
    pub client_secret_env: &'static str,
    pub extra_auth_params: &'static [(&'static str, &'static str)],
    /// Also accept `{ token }` (a PAT / bot token) instead of the OAuth dance.
    pub accepts_token: bool,
}

impl OauthConfig {
    pub fn client(&self) -> Option<(String, String)> {
        let id = std::env::var(self.client_id_env)
            .ok()
            .filter(|s| !s.is_empty())?;
        let secret = std::env::var(self.client_secret_env)
            .ok()
            .filter(|s| !s.is_empty())?;
        Some((id, secret))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionKind {
    Read,
    Write,
    Send,
}

impl ActionKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ActionKind::Read => "read",
            ActionKind::Write => "write",
            ActionKind::Send => "send",
        }
    }

    /// Write and send actions need the user's approval on the box side (F2 gate).
    pub fn needs_approval(self) -> bool {
        match self {
            ActionKind::Read => false,
            ActionKind::Write | ActionKind::Send => true,
        }
    }
}

pub struct ArgSpec {
    pub name: &'static str,
    pub ty: &'static str,
    pub required: bool,
    pub help: &'static str,
}

pub struct ActionSpec {
    pub name: &'static str,
    pub kind: ActionKind,
    pub help: &'static str,
    pub args: &'static [ArgSpec],
}

impl ActionSpec {
    pub fn to_json(&self) -> Value {
        let props: serde_json::Map<String, Value> = self
            .args
            .iter()
            .map(|a| {
                (
                    a.name.to_owned(),
                    json!({ "type": a.ty, "description": a.help }),
                )
            })
            .collect();
        let required: Vec<&str> = self
            .args
            .iter()
            .filter(|a| a.required)
            .map(|a| a.name)
            .collect();
        json!({
            "name": self.name,
            "kind": self.kind.as_str(),
            "description": self.help,
            "args": { "type": "object", "properties": props, "required": required },
        })
    }

    /// Missing required args, by name.
    pub fn missing(&self, args: &Value) -> Vec<&'static str> {
        self.args
            .iter()
            .filter(|a| a.required && args.get(a.name).is_none_or(Value::is_null))
            .map(|a| a.name)
            .collect()
    }
}

const fn req(name: &'static str, ty: &'static str, help: &'static str) -> ArgSpec {
    ArgSpec {
        name,
        ty,
        required: true,
        help,
    }
}

const fn opt(name: &'static str, ty: &'static str, help: &'static str) -> ArgSpec {
    ArgSpec {
        name,
        ty,
        required: false,
        help,
    }
}

const GOOGLE_ACTIONS: &[ActionSpec] = &[
    ActionSpec {
        name: "list_threads",
        kind: ActionKind::Read,
        help: "Recent Gmail threads (inbox by default).",
        args: &[
            opt("label", "string", "Gmail label id, default INBOX"),
            opt("max", "integer", "max results, default 20"),
        ],
    },
    ActionSpec {
        name: "search",
        kind: ActionKind::Read,
        help: "Search Gmail with Gmail query syntax.",
        args: &[
            req(
                "q",
                "string",
                "Gmail search query, e.g. from:alice is:unread",
            ),
            opt("max", "integer", "max results, default 20"),
        ],
    },
    ActionSpec {
        name: "read_message",
        kind: ActionKind::Read,
        help: "Read one Gmail message (headers + text body).",
        args: &[req("id", "string", "message id")],
    },
    ActionSpec {
        name: "create_draft",
        kind: ActionKind::Write,
        help: "Create a Gmail draft (not sent).",
        args: &[
            req("to", "string", "recipient"),
            req("subject", "string", "subject"),
            req("body", "string", "plain-text body"),
        ],
    },
    ActionSpec {
        name: "send",
        kind: ActionKind::Send,
        help: "Send an email from the user's Gmail.",
        args: &[
            req("to", "string", "recipient"),
            req("subject", "string", "subject"),
            req("body", "string", "plain-text body"),
        ],
    },
    ActionSpec {
        name: "list_events",
        kind: ActionKind::Read,
        help: "Upcoming events on the primary calendar.",
        args: &[
            opt("time_min", "string", "RFC 3339 lower bound, default now"),
            opt("max", "integer", "max results, default 20"),
        ],
    },
    ActionSpec {
        name: "create_event",
        kind: ActionKind::Send,
        help: "Create a calendar event (attendees are invited).",
        args: &[
            req("summary", "string", "title"),
            req("start", "string", "RFC 3339 start"),
            req("end", "string", "RFC 3339 end"),
            opt("attendees", "array", "email addresses"),
            opt("description", "string", "description"),
        ],
    },
];

const SLACK_ACTIONS: &[ActionSpec] = &[
    ActionSpec {
        name: "list_channels",
        kind: ActionKind::Read,
        help: "Channels the user is in.",
        args: &[opt("limit", "integer", "max results, default 100")],
    },
    ActionSpec {
        name: "read_channel",
        kind: ActionKind::Read,
        help: "Recent messages of a channel.",
        args: &[
            req("channel", "string", "channel id"),
            opt("limit", "integer", "max messages, default 50"),
        ],
    },
    ActionSpec {
        name: "search",
        kind: ActionKind::Read,
        help: "Search messages.",
        args: &[
            req("query", "string", "search query"),
            opt("count", "integer", "max results, default 20"),
        ],
    },
    ActionSpec {
        name: "post_message",
        kind: ActionKind::Send,
        help: "Post a message as the user.",
        args: &[
            req("channel", "string", "channel id"),
            req("text", "string", "message text"),
        ],
    },
];

const GITHUB_ACTIONS: &[ActionSpec] = &[
    ActionSpec {
        name: "list_repos",
        kind: ActionKind::Read,
        help: "Repositories the user can access, most recently updated first.",
        args: &[opt("per_page", "integer", "max results, default 30")],
    },
    ActionSpec {
        name: "list_issues",
        kind: ActionKind::Read,
        help: "Issues of a repository.",
        args: &[
            req("repo", "string", "owner/name"),
            opt("state", "string", "open|closed|all, default open"),
        ],
    },
    ActionSpec {
        name: "read_issue",
        kind: ActionKind::Read,
        help: "One issue with its body.",
        args: &[
            req("repo", "string", "owner/name"),
            req("number", "integer", "issue number"),
        ],
    },
    ActionSpec {
        name: "list_prs",
        kind: ActionKind::Read,
        help: "Pull requests of a repository.",
        args: &[
            req("repo", "string", "owner/name"),
            opt("state", "string", "open|closed|all, default open"),
        ],
    },
    ActionSpec {
        name: "create_issue",
        kind: ActionKind::Write,
        help: "Open a new issue.",
        args: &[
            req("repo", "string", "owner/name"),
            req("title", "string", "title"),
            opt("body", "string", "Markdown body"),
        ],
    },
    ActionSpec {
        name: "comment",
        kind: ActionKind::Send,
        help: "Comment on an issue or pull request.",
        args: &[
            req("repo", "string", "owner/name"),
            req("number", "integer", "issue or PR number"),
            req("body", "string", "Markdown body"),
        ],
    },
];

const HARNESS_ACTIONS: &[ActionSpec] = &[
    ActionSpec {
        name: "status",
        kind: ActionKind::Read,
        help: "Service policy, identity, queue counts.",
        args: &[],
    },
    ActionSpec {
        name: "accounts",
        kind: ActionKind::Read,
        help: "Connected networks and Gmail status.",
        args: &[],
    },
    ActionSpec {
        name: "search",
        kind: ActionKind::Read,
        help: "Search the Gmail inbox (Gmail query syntax).",
        args: &[req("query", "string", "e.g. is:unread from:alice")],
    },
    ActionSpec {
        name: "read",
        kind: ActionKind::Read,
        help: "Read one Gmail message by uid.",
        args: &[req("uid", "string", "message uid from search")],
    },
    ActionSpec {
        name: "search_conversations",
        kind: ActionKind::Read,
        help: "Find Beeper chats (WhatsApp, Telegram, Discord…) by name.",
        args: &[req("query", "string", "contact or group name")],
    },
    ActionSpec {
        name: "read_chat",
        kind: ActionKind::Read,
        help: "Recent messages of one Beeper chat.",
        args: &[req(
            "chat_id",
            "string",
            "exact chat id from search_conversations",
        )],
    },
    ActionSpec {
        name: "draft",
        kind: ActionKind::Write,
        help: "Register a draft (nothing is sent).",
        args: &[
            req("text", "string", "message text"),
            opt("intent", "string", "one-line intent"),
        ],
    },
    ActionSpec {
        name: "send",
        kind: ActionKind::Send,
        help: "Queue a send: email (to + subject) or chat (chat_id).",
        args: &[
            req("text", "string", "message text"),
            opt("to", "string", "email recipient"),
            opt("subject", "string", "email subject"),
            opt("chat_id", "string", "Beeper chat id"),
            opt("intent", "string", "one-line intent"),
            opt("request_id", "string", "idempotency key; reuse on retry"),
        ],
    },
    ActionSpec {
        name: "request_status",
        kind: ActionKind::Read,
        help: "Delivery state of a queued send.",
        args: &[req("request_id", "string", "request id")],
    },
];

/// Everything the catalog endpoint says about one provider.
pub fn provider_json(p: Provider, configured: bool, mock: bool) -> Value {
    let (connect, scopes): (&str, Vec<&str>) = match p.auth() {
        Auth::Oauth(o) => (
            if o.accepts_token {
                "oauth_or_token"
            } else {
                "oauth"
            },
            o.scopes.to_vec(),
        ),
        Auth::Token => ("token", vec![]),
    };
    json!({
        "provider": p.as_str(),
        "label": p.label(),
        "connect": connect,
        "oauth_configured": configured,
        "mock": mock,
        "scopes": scopes,
        "actions": p.actions().iter().map(ActionSpec::to_json).collect::<Vec<_>>(),
    })
}

/// A prepared upstream request; the broker adds the bearer token and executes it.
pub struct Upstream {
    pub method: Method,
    pub url: String,
    pub query: Vec<(String, String)>,
    pub json: Option<Value>,
    pub headers: Vec<(&'static str, String)>,
}

fn s<'a>(args: &'a Value, k: &str) -> Option<&'a str> {
    args.get(k).and_then(Value::as_str)
}

fn n(args: &Value, k: &str, default: i64) -> i64 {
    args.get(k).and_then(Value::as_i64).unwrap_or(default)
}

fn rfc822(to: &str, subject: &str, body: &str) -> String {
    let raw = format!(
        "To: {to}\r\nSubject: {subject}\r\nContent-Type: text/plain; charset=\"UTF-8\"\r\nMIME-Version: 1.0\r\n\r\n{body}"
    );
    B64URL.encode(raw.as_bytes())
}

fn repo_path(repo: &str) -> Result<String, String> {
    let (owner, name) = repo
        .split_once('/')
        .ok_or_else(|| "repo must be owner/name".to_owned())?;
    if owner.is_empty()
        || name.is_empty()
        || !repo
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/'))
    {
        return Err("repo must be owner/name".into());
    }
    Ok(format!("/repos/{owner}/{name}"))
}

/// Build the upstream request for `provider.action(args)`; `config` is the connector's config
/// (e.g. `baseUrl` for `harness_comms`).
pub fn build(
    provider: Provider,
    action: &ActionSpec,
    args: &Value,
    config: &Value,
) -> Result<Upstream, String> {
    let get = |url: String| Upstream {
        method: Method::GET,
        url,
        query: Vec::new(),
        json: None,
        headers: Vec::new(),
    };
    let post = |url: String, body: Value| Upstream {
        method: Method::POST,
        url,
        query: Vec::new(),
        json: Some(body),
        headers: Vec::new(),
    };
    let q = |mut u: Upstream, k: &str, v: String| {
        u.query.push((k.to_owned(), v));
        u
    };
    match provider {
        Provider::Google => {
            const GMAIL: &str = "https://gmail.googleapis.com/gmail/v1/users/me";
            const CAL: &str = "https://www.googleapis.com/calendar/v3/calendars/primary";
            match action.name {
                "list_threads" => {
                    let u = get(format!("{GMAIL}/threads"));
                    let u = q(
                        u,
                        "labelIds",
                        s(args, "label").unwrap_or("INBOX").to_owned(),
                    );
                    Ok(q(
                        u,
                        "maxResults",
                        n(args, "max", 20).clamp(1, 100).to_string(),
                    ))
                }
                "search" => {
                    let u = get(format!("{GMAIL}/messages"));
                    let u = q(u, "q", s(args, "q").unwrap_or_default().to_owned());
                    Ok(q(
                        u,
                        "maxResults",
                        n(args, "max", 20).clamp(1, 100).to_string(),
                    ))
                }
                "read_message" => {
                    let u = get(format!(
                        "{GMAIL}/messages/{}",
                        s(args, "id").unwrap_or_default()
                    ));
                    Ok(q(u, "format", "full".into()))
                }
                "create_draft" => Ok(post(
                    format!("{GMAIL}/drafts"),
                    json!({ "message": { "raw": rfc822(s(args, "to").unwrap_or_default(), s(args, "subject").unwrap_or_default(), s(args, "body").unwrap_or_default()) } }),
                )),
                "send" => Ok(post(
                    format!("{GMAIL}/messages/send"),
                    json!({ "raw": rfc822(s(args, "to").unwrap_or_default(), s(args, "subject").unwrap_or_default(), s(args, "body").unwrap_or_default()) }),
                )),
                "list_events" => {
                    let u = get(format!("{CAL}/events"));
                    let u = q(u, "singleEvents", "true".into());
                    let u = q(u, "orderBy", "startTime".into());
                    let time_min = s(args, "time_min")
                        .map(str::to_owned)
                        .unwrap_or_else(|| chrono::Utc::now().to_rfc3339());
                    let u = q(u, "timeMin", time_min);
                    Ok(q(
                        u,
                        "maxResults",
                        n(args, "max", 20).clamp(1, 250).to_string(),
                    ))
                }
                "create_event" => {
                    let attendees: Vec<Value> = args
                        .get("attendees")
                        .and_then(Value::as_array)
                        .map(|a| {
                            a.iter()
                                .filter_map(Value::as_str)
                                .map(|e| json!({ "email": e }))
                                .collect()
                        })
                        .unwrap_or_default();
                    let u = post(
                        format!("{CAL}/events"),
                        json!({
                            "summary": s(args, "summary").unwrap_or_default(),
                            "description": s(args, "description").unwrap_or_default(),
                            "start": { "dateTime": s(args, "start").unwrap_or_default() },
                            "end": { "dateTime": s(args, "end").unwrap_or_default() },
                            "attendees": attendees,
                        }),
                    );
                    Ok(q(u, "sendUpdates", "all".into()))
                }
                other => Err(format!("google has no action {other}")),
            }
        }
        Provider::Slack => {
            const API: &str = "https://slack.com/api";
            match action.name {
                "list_channels" => {
                    let u = get(format!("{API}/conversations.list"));
                    let u = q(u, "types", "public_channel,private_channel".into());
                    let u = q(u, "exclude_archived", "true".into());
                    Ok(q(
                        u,
                        "limit",
                        n(args, "limit", 100).clamp(1, 1000).to_string(),
                    ))
                }
                "read_channel" => {
                    let u = get(format!("{API}/conversations.history"));
                    let u = q(
                        u,
                        "channel",
                        s(args, "channel").unwrap_or_default().to_owned(),
                    );
                    Ok(q(
                        u,
                        "limit",
                        n(args, "limit", 50).clamp(1, 200).to_string(),
                    ))
                }
                "search" => {
                    let u = get(format!("{API}/search.messages"));
                    let u = q(u, "query", s(args, "query").unwrap_or_default().to_owned());
                    Ok(q(
                        u,
                        "count",
                        n(args, "count", 20).clamp(1, 100).to_string(),
                    ))
                }
                "post_message" => Ok(post(
                    format!("{API}/chat.postMessage"),
                    json!({ "channel": s(args, "channel").unwrap_or_default(), "text": s(args, "text").unwrap_or_default() }),
                )),
                other => Err(format!("slack has no action {other}")),
            }
        }
        Provider::Github => {
            const API: &str = "https://api.github.com";
            let hdrs = |mut u: Upstream| {
                u.headers
                    .push(("Accept", "application/vnd.github+json".to_owned()));
                u.headers
                    .push(("X-GitHub-Api-Version", "2022-11-28".to_owned()));
                u
            };
            match action.name {
                "list_repos" => {
                    let u = get(format!("{API}/user/repos"));
                    let u = q(u, "sort", "updated".into());
                    let u = q(
                        u,
                        "per_page",
                        n(args, "per_page", 30).clamp(1, 100).to_string(),
                    );
                    Ok(hdrs(u))
                }
                "list_issues" => {
                    let p = repo_path(s(args, "repo").unwrap_or_default())?;
                    let u = get(format!("{API}{p}/issues"));
                    let u = q(u, "state", s(args, "state").unwrap_or("open").to_owned());
                    Ok(hdrs(q(u, "per_page", "30".into())))
                }
                "read_issue" => {
                    let p = repo_path(s(args, "repo").unwrap_or_default())?;
                    Ok(hdrs(get(format!(
                        "{API}{p}/issues/{}",
                        n(args, "number", 0)
                    ))))
                }
                "list_prs" => {
                    let p = repo_path(s(args, "repo").unwrap_or_default())?;
                    let u = get(format!("{API}{p}/pulls"));
                    let u = q(u, "state", s(args, "state").unwrap_or("open").to_owned());
                    Ok(hdrs(q(u, "per_page", "30".into())))
                }
                "create_issue" => {
                    let p = repo_path(s(args, "repo").unwrap_or_default())?;
                    Ok(hdrs(post(
                        format!("{API}{p}/issues"),
                        json!({ "title": s(args, "title").unwrap_or_default(), "body": s(args, "body").unwrap_or_default() }),
                    )))
                }
                "comment" => {
                    let p = repo_path(s(args, "repo").unwrap_or_default())?;
                    Ok(hdrs(post(
                        format!("{API}{p}/issues/{}/comments", n(args, "number", 0)),
                        json!({ "body": s(args, "body").unwrap_or_default() }),
                    )))
                }
                other => Err(format!("github has no action {other}")),
            }
        }
        Provider::HarnessComms => {
            let base = config
                .get("baseUrl")
                .and_then(Value::as_str)
                .map(|u| u.trim_end_matches('/').to_owned())
                .ok_or_else(|| "harness_comms: no baseUrl in connector config".to_owned())?;
            match action.name {
                "status" => Ok(get(format!("{base}/v1/status"))),
                "accounts" => Ok(get(format!("{base}/v1/accounts"))),
                "search" => Ok(post(
                    format!("{base}/v1/messages/read"),
                    json!({ "target": { "connector": "gmail" }, "query": s(args, "query").unwrap_or_default() }),
                )),
                "read" => Ok(post(
                    format!("{base}/v1/messages/read"),
                    json!({ "target": { "connector": "gmail" }, "uid": s(args, "uid").unwrap_or_default() }),
                )),
                "search_conversations" => Ok(post(
                    format!("{base}/v1/conversations/search"),
                    json!({ "query": s(args, "query").unwrap_or_default() }),
                )),
                "read_chat" => Ok(post(
                    format!("{base}/v1/messages/read"),
                    json!({ "target": { "connector": "beeper", "chat_id": s(args, "chat_id").unwrap_or_default() } }),
                )),
                "draft" => Ok(post(
                    format!("{base}/v1/drafts"),
                    json!({ "text": s(args, "text").unwrap_or_default(), "intent": s(args, "intent").unwrap_or("draft from Arbos") }),
                )),
                "send" => {
                    let request_id = s(args, "request_id")
                        .map(str::to_owned)
                        .unwrap_or_else(|| format!("arbos:{}", arbos_core::ids::ulid()));
                    let target = if let Some(chat) = s(args, "chat_id") {
                        json!({ "connector": "beeper", "chat_id": chat })
                    } else if let Some(to) = s(args, "to") {
                        json!({ "connector": "gmail", "recipient": to })
                    } else {
                        return Err("send needs `to` (email) or `chat_id` (Beeper chat)".into());
                    };
                    let mut body = json!({
                        "request_id": request_id,
                        "target": target,
                        "text": s(args, "text").unwrap_or_default(),
                        "intent": s(args, "intent").unwrap_or("message from Arbos"),
                    });
                    if let Some(subject) = s(args, "subject") {
                        body["subject"] = json!(subject);
                    }
                    Ok(post(format!("{base}/v1/requests"), body))
                }
                "request_status" => Ok(get(format!(
                    "{base}/v1/requests/{}",
                    s(args, "request_id").unwrap_or_default()
                ))),
                other => Err(format!("harness_comms has no action {other}")),
            }
        }
    }
}

/// What a token endpoint (or a pasted token) grants.
pub struct TokenGrant {
    pub access: String,
    pub refresh: Option<String>,
    pub expires_in_s: Option<i64>,
    pub scopes: Vec<String>,
    pub label: String,
}

/// Pull the tokens out of a provider's token-endpoint reply.
pub fn parse_token_response(provider: Provider, v: &Value) -> Result<TokenGrant, String> {
    let str_at = |ptr: &str| v.pointer(ptr).and_then(Value::as_str).map(str::to_owned);
    let scopes_of = |s: Option<String>| -> Vec<String> {
        s.map(|s| {
            s.split([' ', ','])
                .filter(|x| !x.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
    };
    if let Some(err) = v.get("error") {
        if !err.is_null() && err.as_bool() != Some(false) {
            let desc = v
                .get("error_description")
                .and_then(Value::as_str)
                .unwrap_or("");
            return Err(format!("{err} {desc}").trim().to_owned());
        }
    }
    match provider {
        Provider::Slack => {
            let access = str_at("/authed_user/access_token")
                .or_else(|| str_at("/access_token"))
                .ok_or("slack: no access token in reply")?;
            let team = str_at("/team/name").unwrap_or_default();
            Ok(TokenGrant {
                access,
                refresh: str_at("/authed_user/refresh_token"),
                expires_in_s: v.pointer("/authed_user/expires_in").and_then(Value::as_i64),
                scopes: scopes_of(str_at("/authed_user/scope")),
                label: if team.is_empty() {
                    "Slack".into()
                } else {
                    format!("Slack · {team}")
                },
            })
        }
        Provider::Google | Provider::Github | Provider::HarnessComms => {
            let access = str_at("/access_token").ok_or("no access_token in token reply")?;
            Ok(TokenGrant {
                access,
                refresh: str_at("/refresh_token"),
                expires_in_s: v.get("expires_in").and_then(Value::as_i64),
                scopes: scopes_of(str_at("/scope")),
                label: provider.label().to_owned(),
            })
        }
    }
}

/// Make upstream replies easier on the model: decode Gmail bodies, drop noisy fields.
pub fn shape_response(provider: Provider, action: &str, raw: Value) -> Value {
    match (provider, action) {
        (Provider::Google, "read_message") => gmail_simplify(&raw),
        (Provider::Github, "list_repos") => json!(raw
            .as_array()
            .map(|a| a
                .iter()
                .map(|r| json!({
                    "full_name": r["full_name"], "private": r["private"], "description": r["description"],
                    "updated_at": r["updated_at"], "open_issues": r["open_issues_count"], "default_branch": r["default_branch"],
                    "html_url": r["html_url"],
                }))
                .collect::<Vec<_>>())
            .unwrap_or_default()),
        (Provider::Github, "list_issues" | "list_prs") => json!(raw
            .as_array()
            .map(|a| a
                .iter()
                .map(|r| json!({
                    "number": r["number"], "title": r["title"], "state": r["state"], "user": r["user"]["login"],
                    "updated_at": r["updated_at"], "html_url": r["html_url"], "is_pr": r.get("pull_request").is_some() || r.get("head").is_some(),
                }))
                .collect::<Vec<_>>())
            .unwrap_or_default()),
        (Provider::Github, "read_issue") => json!({
            "number": raw["number"], "title": raw["title"], "state": raw["state"], "user": raw["user"]["login"],
            "body": raw["body"], "comments": raw["comments"], "html_url": raw["html_url"], "updated_at": raw["updated_at"],
        }),
        (Provider::Github, "create_issue" | "comment") => json!({
            "id": raw["id"], "number": raw["number"], "html_url": raw["html_url"], "created_at": raw["created_at"],
        }),
        _ => raw,
    }
}

fn gmail_header<'a>(msg: &'a Value, name: &str) -> &'a str {
    msg.pointer("/payload/headers")
        .and_then(Value::as_array)
        .and_then(|h| {
            h.iter().find(|x| {
                x["name"]
                    .as_str()
                    .is_some_and(|n| n.eq_ignore_ascii_case(name))
            })
        })
        .and_then(|x| x["value"].as_str())
        .unwrap_or("")
}

fn gmail_body(part: &Value, out: &mut String) {
    let mime = part["mimeType"].as_str().unwrap_or("");
    if mime == "text/plain" {
        if let Some(data) = part.pointer("/body/data").and_then(Value::as_str) {
            if let Ok(bytes) = B64URL.decode(data).or_else(|_| B64.decode(data)) {
                out.push_str(&String::from_utf8_lossy(&bytes));
            }
        }
    }
    if let Some(parts) = part["parts"].as_array() {
        for p in parts {
            gmail_body(p, out);
        }
    }
}

fn gmail_simplify(msg: &Value) -> Value {
    let mut body = String::new();
    if let Some(p) = msg.get("payload") {
        gmail_body(p, &mut body);
    }
    json!({
        "id": msg["id"], "threadId": msg["threadId"], "labelIds": msg["labelIds"],
        "from": gmail_header(msg, "From"), "to": gmail_header(msg, "To"), "subject": gmail_header(msg, "Subject"),
        "date": gmail_header(msg, "Date"), "snippet": msg["snippet"],
        "body": body.chars().take(20_000).collect::<String>(),
    })
}

/// Canned reply for mock mode. Sends/writes are recorded by the caller in `connector_outbox`.
pub fn mock_response(provider: Provider, action: &ActionSpec, args: &Value) -> Value {
    let now = now_ms();
    match (provider, action.name) {
        (Provider::Google, "list_threads") | (Provider::Google, "search") => {
            json!({ "mock": true, "messages": [
            { "id": "18f0mock0001", "threadId": "18f0mock0001", "from": "Louisa Marin <louisa@example.com>", "subject": "Friday plan?", "snippet": "Does Friday still work for the kick-off? I can move things if not." },
            { "id": "18f0mock0002", "threadId": "18f0mock0002", "from": "Daytona Billing <billing@daytona.io>", "subject": "Your October invoice", "snippet": "Invoice #4471 for $23.80 is ready." },
        ] })
        }
        (Provider::Google, "read_message") => {
            json!({ "mock": true, "id": s(args, "id").unwrap_or("18f0mock0001"), "from": "Louisa Marin <louisa@example.com>", "to": "you@example.com", "subject": "Friday plan?", "date": "Thu, 15 Oct 2026 09:12:00 +0000", "body": "Hi,\n\nDoes Friday still work for the kick-off? I can move things if not.\n\nLouisa" })
        }
        (Provider::Google, "create_draft") => {
            json!({ "mock": true, "id": "r-draft-mock", "message": { "id": "18f0mockdraft", "labelIds": ["DRAFT"] } })
        }
        (Provider::Google, "send") => {
            json!({ "mock": true, "id": "18f0mocksent", "labelIds": ["SENT"], "to": s(args, "to") })
        }
        (Provider::Google, "list_events") => json!({ "mock": true, "items": [
            { "id": "evt-mock-1", "summary": "Kick-off with Louisa", "start": { "dateTime": "2026-10-16T10:00:00Z" }, "end": { "dateTime": "2026-10-16T10:30:00Z" } },
            { "id": "evt-mock-2", "summary": "Relay deploy window", "start": { "dateTime": "2026-10-17T14:00:00Z" }, "end": { "dateTime": "2026-10-17T15:00:00Z" } },
        ] }),
        (Provider::Google, "create_event") => {
            json!({ "mock": true, "id": "evt-mock-new", "status": "confirmed", "summary": s(args, "summary"), "htmlLink": "https://calendar.google.com/event?eid=mock" })
        }
        (Provider::Slack, "list_channels") => json!({ "mock": true, "ok": true, "channels": [
            { "id": "C0MOCKGEN", "name": "general", "num_members": 12 },
            { "id": "C0MOCKENG", "name": "eng", "num_members": 5 },
        ] }),
        (Provider::Slack, "read_channel") => json!({ "mock": true, "ok": true, "messages": [
            { "user": "U0MOCKA", "text": "Relay deploy is green.", "ts": "1760518800.000100" },
            { "user": "U0MOCKB", "text": "Nice — shipping the connectors demo next.", "ts": "1760518860.000200" },
        ] }),
        (Provider::Slack, "search") => {
            json!({ "mock": true, "ok": true, "messages": { "total": 1, "matches": [ { "channel": { "id": "C0MOCKENG", "name": "eng" }, "text": format!("… {} …", s(args, "query").unwrap_or_default()), "ts": "1760518860.000200" } ] } })
        }
        (Provider::Slack, "post_message") => {
            json!({ "mock": true, "ok": true, "channel": s(args, "channel"), "ts": format!("{}.000300", now / 1000) })
        }
        (Provider::Github, "list_repos") => json!({ "mock": true, "repos": [
            { "full_name": "unarbos/arbos", "private": true, "description": "Arbos monorepo", "updated_at": "2026-10-15T04:00:00Z", "open_issues": 3 },
            { "full_name": "unarbos/harness-comms", "private": true, "description": "Comms API", "updated_at": "2026-10-14T18:00:00Z", "open_issues": 0 },
        ] }),
        (Provider::Github, "list_issues") | (Provider::Github, "list_prs") => {
            json!({ "mock": true, "repo": s(args, "repo"), "items": [
            { "number": 42, "title": "Connectors: Google OAuth client missing", "state": "open", "user": "unarbos" },
            { "number": 41, "title": "Relay: encrypt connector tokens", "state": "open", "user": "unarbos" },
        ] })
        }
        (Provider::Github, "read_issue") => {
            json!({ "mock": true, "repo": s(args, "repo"), "number": n(args, "number", 42), "title": "Connectors: Google OAuth client missing", "state": "open", "body": "We need a Google Cloud OAuth client (desktop type) with the Gmail and Calendar scopes." })
        }
        (Provider::Github, "create_issue") => {
            json!({ "mock": true, "number": 43, "html_url": format!("https://github.com/{}/issues/43", s(args, "repo").unwrap_or("owner/repo")) })
        }
        (Provider::Github, "comment") => {
            json!({ "mock": true, "id": 900001, "html_url": format!("https://github.com/{}/issues/{}#issuecomment-900001", s(args, "repo").unwrap_or("owner/repo"), n(args, "number", 0)) })
        }
        (Provider::HarnessComms, "status") => {
            json!({ "mock": true, "paused": false, "identity": "arbos-comms-mock", "scopes": ["read", "search", "accounts", "draft", "request_send", "events"], "sending_requires_approval": false })
        }
        (Provider::HarnessComms, "accounts") => {
            json!({ "mock": true, "accounts": [ { "connector": "beeper", "networks": ["WhatsApp", "Telegram", "Discord"] }, { "connector": "gmail", "status": "connected" } ] })
        }
        (Provider::HarnessComms, "search") => {
            json!({ "mock": true, "messages": [ { "uid": "9001", "from": "louisa@example.com", "subject": "Friday plan?", "date": "2026-10-15T09:12:00Z" } ] })
        }
        (Provider::HarnessComms, "read") => {
            json!({ "mock": true, "uid": s(args, "uid"), "from": "louisa@example.com", "subject": "Friday plan?", "text": "Does Friday still work for the kick-off?" })
        }
        (Provider::HarnessComms, "search_conversations") => {
            json!({ "mock": true, "chats": [ { "chat_id": "!mockchat:beeper.local", "network": "WhatsApp", "title": s(args, "query") } ] })
        }
        (Provider::HarnessComms, "read_chat") => {
            json!({ "mock": true, "chat_id": s(args, "chat_id"), "messages": [ { "sender": "Louisa", "text": "Does Friday work?", "ts": now - 3_600_000 } ] })
        }
        (Provider::HarnessComms, "draft") => {
            json!({ "mock": true, "draft_id": "draft-mock-1", "text": s(args, "text") })
        }
        (Provider::HarnessComms, "send") => {
            json!({ "mock": true, "request_id": s(args, "request_id").unwrap_or("arbos:mock"), "status": "queued" })
        }
        (Provider::HarnessComms, "request_status") => {
            json!({ "mock": true, "request_id": s(args, "request_id"), "status": "sent" })
        }
        _ => {
            json!({ "mock": true, "provider": provider.as_str(), "action": action.name, "args": args })
        }
    }
}
