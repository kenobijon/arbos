//! Secret scrubbing shared by the box and the relay (D19). The box runs it at T3 write time
//! (`arbos-agentd::store::redact`, plus the live vault values only that process knows) and
//! before anything leaves in a feedback rollout; the relay runs it over every row it hands to a
//! debugging reader (`GET /projects/{id}/inspect`), so a secret that reached a relay table never
//! reaches a bot's context window.

use serde_json::Value;

/// Known key shapes, `Authorization: Bearer …` and `KEY=value` lines for well-known secret names.
pub fn redact_patterns(text: &str) -> String {
    let mut out = text.to_owned();
    let patterns: [(&str, &str); 8] = [
        (r"sk-or-v1-[A-Za-z0-9]{20,}", "[REDACTED:openrouter-key]"),
        (r"\bops_[A-Za-z0-9]{20,}", "[REDACTED:1password-token]"),
        (r"\bdp\.st\.[A-Za-z0-9_\-]{10,}", "[REDACTED:doppler-token]"),
        (r"sk-ant-[A-Za-z0-9_\-]{20,}", "[REDACTED:anthropic-key]"),
        (r"sk-[A-Za-z0-9]{32,}", "[REDACTED:openai-key]"),
        (r"dtn_[A-Za-z0-9]{20,}", "[REDACTED:daytona-key]"),
        (
            r"(?i)(authorization:\s*bearer\s+)[A-Za-z0-9._\-]+",
            "${1}[REDACTED:bearer]",
        ),
        (
            r"(?i)\b(gh[pousr]_[A-Za-z0-9]{20,}|xox[baprs]-[A-Za-z0-9\-]{10,}|AKIA[0-9A-Z]{16})\b",
            "[REDACTED:token]",
        ),
    ];
    for (pat, rep) in patterns {
        if let Ok(re) = regex_lite::Regex::new(pat) {
            out = re.replace_all(&out, rep).into_owned();
        }
    }
    if let Ok(re) = regex_lite::Regex::new(
        r"(?im)^(\s*(?:export\s+)?[A-Z0-9_]*(?:KEY|TOKEN|SECRET|PASSWORD)[A-Z0-9_]*\s*=\s*)\S+",
    ) {
        out = re.replace_all(&out, "${1}[REDACTED:env]").into_owned();
    }
    out
}

/// Secret placeholders and vault references the secrets system hands an agent
/// (`arbos-secret://…`, `op://…`, `$ARBOS_SECRET_*`): never useful to a reader, always scrubbed.
pub fn redact_placeholders(text: &str) -> String {
    let mut out = text.to_owned();
    let patterns: [(&str, &str); 3] = [
        (r"arbos-secret://[^\s\x22'`<>]+", "[secret placeholder]"),
        (r"\bop://[^\s\x22'`<>]+", "[vault reference]"),
        (r"\$\{?ARBOS_SECRET_[A-Z0-9_]+\}?", "[secret env]"),
    ];
    for (pat, rep) in patterns {
        if let Ok(re) = regex_lite::Regex::new(pat) {
            out = re.replace_all(&out, rep).into_owned();
        }
    }
    out
}

/// Both passes: what a debugging reader may see.
pub fn scrub(text: &str) -> String {
    redact_placeholders(&redact_patterns(text))
}

/// `scrub` over every string inside a JSON value (keys are left alone).
pub fn scrub_value(v: &mut Value) {
    match v {
        Value::String(s) => {
            let clean = scrub(s);
            if clean != *s {
                *s = clean;
            }
        }
        Value::Array(items) => items.iter_mut().for_each(scrub_value),
        Value::Object(map) => map.values_mut().for_each(scrub_value),
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn keys_and_placeholders_go() {
        let text = "key sk-or-v1-abcdefghijklmnopqrstuvwxyz0123456789 and op://Arbos/item/field and \
                    arbos-secret://abc123 and $ARBOS_SECRET_GITHUB and Authorization: Bearer abc.def-ghi";
        let out = scrub(text);
        assert!(out.contains("[REDACTED:openrouter-key]"), "{out}");
        assert!(out.contains("[vault reference]"), "{out}");
        assert!(out.contains("[secret placeholder]"), "{out}");
        assert!(out.contains("[secret env]"), "{out}");
        assert!(out.contains("Bearer [REDACTED:bearer]"), "{out}");
        assert!(!out.contains("abc.def-ghi"), "{out}");
    }

    #[test]
    fn env_lines_keep_the_name() {
        let out = scrub("OPENAI_API_KEY=sk-live-123\nNAME=fine");
        assert_eq!(out, "OPENAI_API_KEY=[REDACTED:env]\nNAME=fine");
    }

    #[test]
    fn values_are_walked() {
        let mut v = json!({ "a": ["ghp_abcdefghijklmnopqrstuvwxyz", { "b": "op://x/y" }], "n": 1 });
        scrub_value(&mut v);
        assert_eq!(v["a"][0], "[REDACTED:token]");
        assert_eq!(v["a"][1]["b"], "[vault reference]");
        assert_eq!(v["n"], 1);
    }
}
