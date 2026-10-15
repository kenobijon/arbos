//! A small markdown block splitter. One transcript row = one block
//! (zeron-study §4.1). Inline: `code`, **bold**, *italic*. Enough for agent
//! replies; swap for pulldown-cmark when tables and links are needed.

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Block {
    Paragraph(String),
    Heading { level: u8, text: String },
    Bullets(Vec<String>),
    Numbered(Vec<String>),
    Code { lang: String, code: String },
    /// GitHub-style pipe table: header row, then body rows.
    Table { header: Vec<String>, rows: Vec<Vec<String>> },
}

pub fn split_blocks(source: &str) -> Vec<Block> {
    let mut blocks = Vec::new();
    let mut lines = source.lines().peekable();
    let mut paragraph: Vec<&str> = Vec::new();

    fn flush(paragraph: &mut Vec<&str>, blocks: &mut Vec<Block>) {
        if !paragraph.is_empty() {
            blocks.push(Block::Paragraph(paragraph.join(" ")));
            paragraph.clear();
        }
    }

    while let Some(line) = lines.next() {
        let trimmed = line.trim_end();
        if trimmed.trim().is_empty() {
            flush(&mut paragraph, &mut blocks);
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("```") {
            flush(&mut paragraph, &mut blocks);
            let lang = rest.trim().to_string();
            let mut code = String::new();
            for code_line in lines.by_ref() {
                if code_line.trim_end().starts_with("```") {
                    break;
                }
                if !code.is_empty() {
                    code.push('\n');
                }
                code.push_str(code_line);
            }
            blocks.push(Block::Code { lang, code });
            continue;
        }
        if trimmed.starts_with('#') {
            let level = trimmed.chars().take_while(|c| *c == '#').count().min(6) as u8;
            let text = trimmed[level as usize..].trim().to_string();
            if !text.is_empty() {
                flush(&mut paragraph, &mut blocks);
                blocks.push(Block::Heading { level, text });
                continue;
            }
        }
        if is_table_row(trimmed) {
            if let Some(next) = lines.peek() {
                if is_table_separator(next.trim_end()) {
                    flush(&mut paragraph, &mut blocks);
                    let header = split_table_row(trimmed);
                    lines.next();
                    let mut rows = Vec::new();
                    while let Some(row) = lines.peek() {
                        let row = row.trim_end();
                        if !is_table_row(row) {
                            break;
                        }
                        rows.push(split_table_row(row));
                        lines.next();
                    }
                    blocks.push(Block::Table { header, rows });
                    continue;
                }
            }
        }
        if let Some(item) = bullet_item(trimmed) {
            flush(&mut paragraph, &mut blocks);
            let mut items = vec![item.to_string()];
            while let Some(next) = lines.peek() {
                match bullet_item(next.trim_end()) {
                    Some(item) => {
                        items.push(item.to_string());
                        lines.next();
                    }
                    None => break,
                }
            }
            blocks.push(Block::Bullets(items));
            continue;
        }
        if let Some(item) = numbered_item(trimmed) {
            flush(&mut paragraph, &mut blocks);
            let mut items = vec![item.to_string()];
            while let Some(next) = lines.peek() {
                match numbered_item(next.trim_end()) {
                    Some(item) => {
                        items.push(item.to_string());
                        lines.next();
                    }
                    None => break,
                }
            }
            blocks.push(Block::Numbered(items));
            continue;
        }
        paragraph.push(trimmed.trim());
    }
    flush(&mut paragraph, &mut blocks);
    blocks
}

fn is_table_row(line: &str) -> bool {
    let t = line.trim();
    t.starts_with('|') && t.ends_with('|') && t.len() > 2
}

fn is_table_separator(line: &str) -> bool {
    let t = line.trim();
    is_table_row(t)
        && t.chars()
            .all(|c| c == '|' || c == '-' || c == ':' || c == ' ')
        && t.contains('-')
}

fn split_table_row(line: &str) -> Vec<String> {
    let t = line.trim();
    let inner = &t[1..t.len() - 1];
    inner.split('|').map(|c| c.trim().to_string()).collect()
}

fn bullet_item(line: &str) -> Option<&str> {
    let t = line.trim_start();
    t.strip_prefix("- ")
        .or_else(|| t.strip_prefix("* "))
        .or_else(|| t.strip_prefix("• "))
}

fn numbered_item(line: &str) -> Option<&str> {
    let t = line.trim_start();
    let digits = t.chars().take_while(|c| c.is_ascii_digit()).count();
    if digits == 0 {
        return None;
    }
    let rest = &t[digits..];
    rest.strip_prefix(". ").or_else(|| rest.strip_prefix(") "))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct InlineStyle {
    pub code: bool,
    pub bold: bool,
    pub italic: bool,
    pub link: bool,
}

/// One inline run: text, style, and the link target when `style.link`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Span {
    pub text: String,
    pub style: InlineStyle,
    pub href: Option<String>,
}

type Styled = (String, InlineStyle, Option<String>);

/// Split inline markdown into styled spans. Unterminated markers are shown
/// literally so a half-streamed `**bold` does not flicker. Links:
/// `[text](url)` and bare `https://…`.
pub fn inline_spans(text: &str) -> Vec<Span> {
    inline_styled(text)
        .into_iter()
        .map(|(text, style, href)| Span { text, style, href })
        .collect()
}

fn inline_styled(text: &str) -> Vec<Styled> {
    let mut spans: Vec<Styled> = Vec::new();
    let style = InlineStyle::default();
    let mut current = String::new();
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;

    let push = |current: &mut String, style: InlineStyle, spans: &mut Vec<Styled>| {
        if !current.is_empty() {
            spans.push((std::mem::take(current), style, None));
        }
    };

    while i < chars.len() {
        let c = chars[i];
        if c == '[' {
            if let Some(close) = find_from(&chars, i + 1, &[']']) {
                if close + 1 < chars.len() && chars[close + 1] == '(' {
                    if let Some(end) = find_from(&chars, close + 2, &[')']) {
                        push(&mut current, style, &mut spans);
                        let label: String = chars[i + 1..close].iter().collect();
                        let href: String = chars[close + 2..end].iter().collect();
                        spans.push((label, InlineStyle { link: true, ..style }, Some(href)));
                        i = end + 1;
                        continue;
                    }
                }
            }
        }
        if c == 'h' && (slice_starts(&chars, i, "https://") || slice_starts(&chars, i, "http://")) {
            let end = (i..chars.len())
                .find(|&j| chars[j].is_whitespace() || chars[j] == ')' || chars[j] == '"')
                .unwrap_or(chars.len());
            let mut url_end = end;
            while url_end > i && matches!(chars[url_end - 1], '.' | ',' | ';' | ':') {
                url_end -= 1;
            }
            push(&mut current, style, &mut spans);
            let url: String = chars[i..url_end].iter().collect();
            spans.push((url.clone(), InlineStyle { link: true, ..style }, Some(url)));
            i = url_end;
            continue;
        }
        if c == '`' {
            if let Some(end) = find_from(&chars, i + 1, &['`']) {
                push(&mut current, style, &mut spans);
                let code: String = chars[i + 1..end].iter().collect();
                spans.push((
                    code,
                    InlineStyle {
                        code: true,
                        ..style
                    },
                    None,
                ));
                i = end + 1;
                continue;
            }
        }
        if c == '*' && i + 1 < chars.len() && chars[i + 1] == '*' {
            if let Some(end) = find_pair(&chars, i + 2) {
                push(&mut current, style, &mut spans);
                let inner: String = chars[i + 2..end].iter().collect();
                for (t, s, href) in inline_styled(&inner) {
                    spans.push((
                        t,
                        InlineStyle {
                            bold: true,
                            ..merge(style, s)
                        },
                        href,
                    ));
                }
                i = end + 2;
                continue;
            }
        }
        if c == '*' || c == '_' {
            let prev_ws = i == 0 || chars[i - 1].is_whitespace();
            if prev_ws {
                if let Some(end) = find_from(&chars, i + 1, &[c]) {
                    if end > i + 1 {
                        push(&mut current, style, &mut spans);
                        let inner: String = chars[i + 1..end].iter().collect();
                        spans.push((
                            inner,
                            InlineStyle {
                                italic: true,
                                ..style
                            },
                            None,
                        ));
                        i = end + 1;
                        continue;
                    }
                }
            }
        }
        current.push(c);
        i += 1;
    }
    push(&mut current, style, &mut spans);
    spans
}

fn merge(a: InlineStyle, b: InlineStyle) -> InlineStyle {
    InlineStyle {
        code: a.code || b.code,
        bold: a.bold || b.bold,
        italic: a.italic || b.italic,
        link: a.link || b.link,
    }
}

fn slice_starts(chars: &[char], at: usize, needle: &str) -> bool {
    let n: Vec<char> = needle.chars().collect();
    at + n.len() <= chars.len() && chars[at..at + n.len()] == n[..]
}

fn find_from(chars: &[char], from: usize, needles: &[char]) -> Option<usize> {
    (from..chars.len()).find(|&i| needles.contains(&chars[i]))
}

fn find_pair(chars: &[char], from: usize) -> Option<usize> {
    (from..chars.len().saturating_sub(1)).find(|&i| chars[i] == '*' && chars[i + 1] == '*')
}
