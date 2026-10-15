//! Skills library shapes (feature F5): a skill is a Markdown how-to with a YAML frontmatter
//! (`name`, `description`, `tags`, `when_to_use`, `version`). The relay stores the fields and
//! the body separately; this module converts between the two representations and estimates
//! prompt size, so the relay, the box, and the clients agree on one format.

use serde::{Deserialize, Serialize};

/// One skill as the relay returns it (`GET /skills/{id}`); the index view carries no `body`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Skill {
    #[serde(default)]
    pub id: String,
    pub slug: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub when_to_use: String,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub origin_project_id: Option<String>,
    #[serde(default)]
    pub shared: bool,
    #[serde(default = "one")]
    pub version: i64,
    #[serde(default)]
    pub created_at: i64,
    #[serde(default)]
    pub updated_at: i64,
}

fn one() -> i64 {
    1
}

/// Rough token count for budgets: four characters per token.
pub fn estimate_tokens(s: &str) -> usize {
    s.chars().count().div_ceil(4)
}

/// `a-z0-9-` slug from a name; empty input gives `skill`.
pub fn slugify(name: &str) -> String {
    let mut out = String::new();
    let mut dash = false;
    for c in name.trim().chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            dash = false;
        } else if !dash && !out.is_empty() {
            out.push('-');
            dash = true;
        }
    }
    let out = out.trim_end_matches('-').to_owned();
    if out.is_empty() {
        "skill".to_owned()
    } else {
        out.chars().take(64).collect()
    }
}

impl Skill {
    /// The Markdown document: YAML frontmatter followed by the body.
    pub fn to_markdown(&self) -> String {
        let mut s = String::from("---\n");
        s.push_str(&format!("name: {}\n", yaml_scalar(&self.name)));
        s.push_str(&format!(
            "description: {}\n",
            yaml_scalar(&self.description)
        ));
        s.push_str(&format!(
            "tags: [{}]\n",
            self.tags
                .iter()
                .map(|t| yaml_scalar(t))
                .collect::<Vec<_>>()
                .join(", ")
        ));
        s.push_str(&format!(
            "when_to_use: {}\n",
            yaml_scalar(&self.when_to_use)
        ));
        s.push_str(&format!("version: {}\n", self.version));
        s.push_str("---\n\n");
        s.push_str(self.body.trim_end());
        s.push('\n');
        s
    }

    /// Parse a Markdown document with the frontmatter above. Missing keys stay empty; a document
    /// without frontmatter is all body and the name comes from the first `#` heading.
    pub fn from_markdown(md: &str) -> Skill {
        let mut skill = Skill {
            version: 1,
            ..Skill::default()
        };
        let text = md.trim_start_matches('\u{feff}');
        let Some(rest) = text.strip_prefix("---") else {
            skill.body = text.trim().to_owned();
            skill.name = first_heading(text).unwrap_or_default();
            skill.slug = slugify(&skill.name);
            return skill;
        };
        let rest = rest
            .trim_start_matches([' ', '\t'])
            .trim_start_matches(['\r', '\n']);
        let Some(end) = rest.find("\n---") else {
            skill.body = text.trim().to_owned();
            skill.name = first_heading(text).unwrap_or_default();
            skill.slug = slugify(&skill.name);
            return skill;
        };
        let (front, after) = rest.split_at(end);
        let after = after.trim_start_matches('\n').trim_start_matches("---");
        skill.body = after.trim().to_owned();
        for line in front.lines() {
            let Some((k, v)) = line.split_once(':') else {
                continue;
            };
            let v = v.trim();
            match k.trim() {
                "name" => skill.name = unquote(v),
                "description" => skill.description = unquote(v),
                "when_to_use" => skill.when_to_use = unquote(v),
                "slug" => skill.slug = slugify(&unquote(v)),
                "version" => skill.version = v.parse().unwrap_or(1),
                "shared" => skill.shared = v == "true",
                "tags" => {
                    skill.tags = v
                        .trim_start_matches('[')
                        .trim_end_matches(']')
                        .split(',')
                        .map(|t| unquote(t.trim()))
                        .filter(|t| !t.is_empty())
                        .collect()
                }
                _ => {}
            }
        }
        if skill.name.is_empty() {
            skill.name = first_heading(&skill.body).unwrap_or_default();
        }
        if skill.slug.is_empty() {
            skill.slug = slugify(&skill.name);
        }
        skill
    }

    /// Text the relevance selector matches against (name, tags, when-to-use, description).
    pub fn match_text(&self) -> String {
        format!(
            "{} {} {} {}",
            self.name,
            self.tags.join(" "),
            self.when_to_use,
            self.description
        )
    }
}

fn first_heading(text: &str) -> Option<String> {
    text.lines()
        .find_map(|l| l.strip_prefix('#'))
        .map(|l| l.trim_start_matches('#').trim().to_owned())
        .filter(|s| !s.is_empty())
}

fn unquote(v: &str) -> String {
    let v = v.trim();
    let inner = v
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .or_else(|| v.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')))
        .unwrap_or(v);
    inner.replace("\\\"", "\"")
}

fn yaml_scalar(v: &str) -> String {
    let one_line = v.split_whitespace().collect::<Vec<_>>().join(" ");
    format!("\"{}\"", one_line.replace('"', "\\\""))
}
