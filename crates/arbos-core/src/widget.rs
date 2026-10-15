//! Question widgets (design §10 "Notifications, question widgets, forms"):
//! the typed shapes `ask_user(question, widget?)` may carry, the `question`
//! doc part they live in, and the one rendering of an answer to text that the
//! model reads. Shared by agentd (render), arbosctl (answer from the CLI) and
//! the Mac app (cards).

use chrono::{Datelike, Local, NaiveDate};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Forms are capped so a card never turns into a page (design §10).
pub const FORM_MAX_FIELDS: usize = 8;

/// JSON schema for the `widget` argument of `ask_user`, shared by the MCP
/// server and the hook API so every harness advertises the same shape.
pub const WIDGET_SCHEMA_JSON: &str = r#"{"type":"object","description":"Optional typed widget. Omit for a plain text question.","properties":{"type":{"type":"string","enum":["choice","multi_select","text","date","form"]},"options":{"type":"array","description":"choice | multi_select","items":{"type":"object","properties":{"id":{"type":"string"},"label":{"type":"string"}},"required":["id","label"]}},"placeholder":{"type":"string"},"multiline":{"type":"boolean"},"min":{"type":"string","description":"date: ISO yyyy-mm-dd"},"max":{"type":"string"},"fields":{"type":"array","description":"form: <= 8 fields","items":{"type":"object","properties":{"id":{"type":"string"},"label":{"type":"string"},"type":{"type":"string","enum":["text","number","choice","date","bool"]},"options":{"type":"array","items":{"type":"object","properties":{"id":{"type":"string"},"label":{"type":"string"}},"required":["id","label"]}},"required":{"type":"boolean"}},"required":["id","label","type"]}}},"required":["type"]}"#;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChoiceOption {
    pub id: String,
    pub label: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldKind {
    Text,
    Number,
    Choice,
    Date,
    Bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FormField {
    pub id: String,
    pub label: String,
    #[serde(rename = "type")]
    pub kind: FieldKind,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<ChoiceOption>,
    #[serde(default)]
    pub required: bool,
}

/// The five widget types. `type` is the tag; unknown types fail to parse and
/// agentd falls back to a plain question, so an old client never sees a shape
/// it cannot draw.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Widget {
    Choice {
        options: Vec<ChoiceOption>,
    },
    MultiSelect {
        options: Vec<ChoiceOption>,
    },
    Text {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        placeholder: Option<String>,
        #[serde(default)]
        multiline: bool,
    },
    Date {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        min: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max: Option<String>,
    },
    Form {
        fields: Vec<FormField>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuestionState {
    Open,
    Answered,
}

impl QuestionState {
    pub fn as_str(self) -> &'static str {
        match self {
            QuestionState::Open => "open",
            QuestionState::Answered => "answered",
        }
    }
}

impl Widget {
    /// Parse and validate a `widget` argument. `None` input is a plain question.
    pub fn parse(v: Option<&Value>) -> Result<Option<Widget>, String> {
        let Some(v) = v else {
            return Ok(None);
        };
        if v.is_null() {
            return Ok(None);
        }
        let w: Widget = serde_json::from_value(v.clone()).map_err(|e| format!("widget: {e}"))?;
        w.validate()?;
        Ok(Some(w))
    }

    pub fn validate(&self) -> Result<(), String> {
        match self {
            Widget::Choice { options } | Widget::MultiSelect { options } => {
                if options.is_empty() {
                    return Err("widget: options must not be empty".into());
                }
                if options.iter().any(|o| o.id.trim().is_empty()) {
                    return Err("widget: every option needs an id".into());
                }
                Ok(())
            }
            Widget::Text { .. } => Ok(()),
            Widget::Date { min, max } => {
                for d in [min, max].into_iter().flatten() {
                    if parse_date(d).is_none() {
                        return Err(format!("widget: date bound {d:?} is not yyyy-mm-dd"));
                    }
                }
                Ok(())
            }
            Widget::Form { fields } => {
                if fields.is_empty() {
                    return Err("widget: form needs at least one field".into());
                }
                if fields.len() > FORM_MAX_FIELDS {
                    return Err(format!(
                        "widget: form has more than {FORM_MAX_FIELDS} fields"
                    ));
                }
                for f in fields {
                    if f.id.trim().is_empty() {
                        return Err("widget: every field needs an id".into());
                    }
                    if f.kind == FieldKind::Choice && f.options.is_empty() {
                        return Err(format!("widget: choice field {:?} has no options", f.id));
                    }
                }
                Ok(())
            }
        }
    }

    pub fn type_name(&self) -> &'static str {
        match self {
            Widget::Choice { .. } => "choice",
            Widget::MultiSelect { .. } => "multi_select",
            Widget::Text { .. } => "text",
            Widget::Date { .. } => "date",
            Widget::Form { .. } => "form",
        }
    }

    /// The text the model (and the user's bubble) sees for an answer value:
    /// `Answer: Label` for one value, a bullet list for forms. Unknown ids
    /// echo as given so a hand-typed answer is never lost.
    pub fn answer_text(&self, value: &Value) -> String {
        match self {
            Widget::Choice { options } => {
                format!("Answer: {}", label_for(options, value))
            }
            Widget::MultiSelect { options } => {
                let picked: Vec<String> = match value {
                    Value::Array(items) => items.iter().map(|v| label_for(options, v)).collect(),
                    other => vec![label_for(options, other)],
                };
                if picked.is_empty() {
                    "Answer: (nothing selected)".into()
                } else {
                    format!("Answer: {}", picked.join(", "))
                }
            }
            Widget::Text { .. } | Widget::Date { .. } => {
                format!("Answer: {}", scalar_text(value))
            }
            Widget::Form { fields } => {
                let mut lines = vec!["Answer:".to_string()];
                let obj = value.as_object();
                for f in fields {
                    let v = obj.and_then(|o| o.get(&f.id)).unwrap_or(&Value::Null);
                    if v.is_null() && !f.required {
                        continue;
                    }
                    let shown = match f.kind {
                        FieldKind::Choice => label_for(&f.options, v),
                        FieldKind::Bool => match v.as_bool() {
                            Some(true) => "yes".into(),
                            Some(false) => "no".into(),
                            None => scalar_text(v),
                        },
                        FieldKind::Text | FieldKind::Number | FieldKind::Date => scalar_text(v),
                    };
                    lines.push(format!("- {}: {}", f.label, shown));
                }
                lines.join("\n")
            }
        }
    }
}

fn label_for(options: &[ChoiceOption], value: &Value) -> String {
    let id = scalar_text(value);
    options
        .iter()
        .find(|o| o.id == id)
        .map(|o| o.label.clone())
        .unwrap_or(id)
}

fn scalar_text(value: &Value) -> String {
    match value {
        Value::Null => "(empty)".into(),
        Value::String(s) if s.trim().is_empty() => "(empty)".into(),
        Value::String(s) => s.clone(),
        Value::Bool(b) => if *b { "yes" } else { "no" }.into(),
        Value::Number(n) => n.to_string(),
        Value::Array(_) | Value::Object(_) => value.to_string(),
    }
}

/// `yyyy-mm-dd` → `(year, month, day)` with a calendar check; no time zones.
pub fn parse_date(s: &str) -> Option<(i32, u32, u32)> {
    let s = s.trim();
    if s.len() != 10 {
        return None;
    }
    let d = NaiveDate::parse_from_str(s, "%Y-%m-%d").ok()?;
    Some((d.year(), d.month(), d.day()))
}

/// Today's local date as `yyyy-mm-dd` (date widget quick picks).
pub fn today_iso() -> String {
    Local::now().date_naive().format("%Y-%m-%d").to_string()
}

/// `iso + days`, or `None` when `iso` is not a date.
pub fn add_days(iso: &str, days: i64) -> Option<String> {
    let d = NaiveDate::parse_from_str(iso.trim(), "%Y-%m-%d").ok()?;
    d.checked_add_signed(chrono::Duration::days(days))
        .map(|d| d.format("%Y-%m-%d").to_string())
}

/// Validate a date answer against the widget bounds (ISO strings compare lexically).
pub fn check_date(iso: &str, min: Option<&str>, max: Option<&str>) -> Result<String, String> {
    let iso = iso.trim();
    if parse_date(iso).is_none() {
        return Err("Use yyyy-mm-dd".into());
    }
    if let Some(min) = min {
        if iso < min {
            return Err(format!("Not before {min}"));
        }
    }
    if let Some(max) = max {
        if iso > max {
            return Err(format!("Not after {max}"));
        }
    }
    Ok(iso.to_owned())
}
