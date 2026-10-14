//! The `route` tool: schema and validator (design §6.2). agentd validates
//! the arguments, retries once with the validator's message, then falls back to `reply`.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RouteAction {
    Reply,
    Act,
    Wake,
    New,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Harness {
    Prime,
    ClaudeCode,
    Pi,
}

impl Harness {
    pub fn as_str(self) -> &'static str {
        match self {
            Harness::Prime => "prime",
            Harness::ClaudeCode => "claude_code",
            Harness::Pi => "pi",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Route {
    pub action: Option<RouteAction>,
    #[serde(default)]
    pub subagent_id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub purpose: Option<String>,
    #[serde(default)]
    pub brief: Option<String>,
    #[serde(default)]
    pub plan: Option<String>,
    #[serde(default)]
    pub harness: Option<Harness>,
    #[serde(default)]
    pub isolate: bool,
    #[serde(default)]
    pub data_heavy: bool,
    #[serde(default)]
    pub reply: Option<String>,
}

/// A validated route. Each variant carries exactly the fields its action needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValidRoute {
    Reply {
        reply: Option<String>,
    },
    Act {
        plan: String,
    },
    Wake {
        subagent_id: String,
        brief: String,
    },
    New {
        name: String,
        purpose: String,
        brief: String,
        harness: Option<Harness>,
        isolate: bool,
        data_heavy: bool,
    },
}

/// JSON schema served to the model as the `route` tool parameters. Byte-stable (design §6.9).
pub const ROUTE_SCHEMA_JSON: &str = r#"{"type":"object","properties":{"action":{"type":"string","enum":["reply","act","wake","new"],"description":"reply: answer now. act: do it yourself in <=8 tool calls / 60s. wake: continue an existing subagent. new: start a subagent."},"subagent_id":{"type":["string","null"],"description":"required for wake"},"name":{"type":["string","null"],"description":"short-name-if-new, e.g. rail-layout"},"purpose":{"type":["string","null"],"description":"one sentence if new"},"brief":{"type":["string","null"],"description":"the exact prompt to send to the subagent, if wake or new"},"plan":{"type":["string","null"],"description":"<= 3 lines: the tool calls you expect to make, if act"},"harness":{"type":["string","null"],"enum":["prime","claude_code","pi",null]},"isolate":{"type":"boolean","default":false},"data_heavy":{"type":"boolean","default":false},"reply":{"type":["string","null"],"description":"the text to stream, if reply"}},"required":["action"],"additionalProperties":false}"#;

fn non_empty(v: &Option<String>) -> Option<String> {
    v.as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

/// Validate raw tool arguments. The error string is what agentd feeds back for the one retry.
pub fn validate(args: &serde_json::Value) -> Result<ValidRoute, String> {
    let r: Route = serde_json::from_value(args.clone())
        .map_err(|e| format!("route arguments do not match the schema: {e}"))?;
    let Some(action) = r.action else {
        return Err("`action` is required and must be one of reply|act|wake|new".to_owned());
    };
    match action {
        RouteAction::Reply => Ok(ValidRoute::Reply {
            reply: non_empty(&r.reply),
        }),
        RouteAction::Act => {
            let plan =
                non_empty(&r.plan).ok_or("`act` requires a non-empty `plan` (<= 3 lines)")?;
            if plan.lines().count() > 3 {
                return Err("`plan` must be at most 3 lines".to_owned());
            }
            Ok(ValidRoute::Act { plan })
        }
        RouteAction::Wake => {
            let subagent_id = non_empty(&r.subagent_id).ok_or("`wake` requires `subagent_id`")?;
            let brief = non_empty(&r.brief).ok_or("`wake` requires a non-empty `brief`")?;
            Ok(ValidRoute::Wake { subagent_id, brief })
        }
        RouteAction::New => {
            let name = non_empty(&r.name).ok_or("`new` requires a short `name`")?;
            if name.len() > 40
                || !name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
            {
                return Err("`name` must be <= 40 chars of [a-zA-Z0-9-_]".to_owned());
            }
            let purpose = non_empty(&r.purpose).ok_or("`new` requires a one-sentence `purpose`")?;
            let brief = non_empty(&r.brief).ok_or("`new` requires a non-empty `brief`")?;
            Ok(ValidRoute::New {
                name,
                purpose,
                brief,
                harness: r.harness,
                isolate: r.isolate,
                data_heavy: r.data_heavy,
            })
        }
    }
}

/// Router rule for the harness when the route left it null (D67).
pub fn pick_harness(
    route_harness: Option<Harness>,
    data_heavy: bool,
    brief: &str,
    subagent_model: &str,
    project_default: Harness,
) -> Harness {
    if let Some(h) = route_harness {
        return h;
    }
    if data_heavy {
        return Harness::Prime;
    }
    let lower = brief.to_ascii_lowercase();
    let repo_wide = [
        "refactor",
        "migration",
        "migrate",
        "implement the plan",
        "across modules",
        "repo-wide",
        "whole repo",
    ]
    .iter()
    .any(|k| lower.contains(k));
    if repo_wide && subagent_model.contains("claude") {
        return Harness::ClaudeCode;
    }
    project_default
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn routing_rules_hold() {
        assert!(matches!(
            validate(&json!({"action":"reply","reply":"hi"})),
            Ok(ValidRoute::Reply { .. })
        ));
        assert!(validate(&json!({"action":"act"})).is_err());
        assert!(validate(&json!({"action":"wake","brief":"x"})).is_err());
        assert!(matches!(
            validate(&json!({"action":"new","name":"rail-layout","purpose":"p","brief":"b"})),
            Ok(ValidRoute::New { .. })
        ));
        assert_eq!(
            pick_harness(None, true, "", "gpt", Harness::Pi),
            Harness::Prime
        );
        assert_eq!(
            pick_harness(
                None,
                false,
                "refactor the auth module",
                "claude-sonnet",
                Harness::Prime
            ),
            Harness::ClaudeCode
        );
        assert_eq!(
            pick_harness(None, false, "fix typo", "claude-sonnet", Harness::Prime),
            Harness::Prime
        );
    }
}
