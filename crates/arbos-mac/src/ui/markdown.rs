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
}

/// Split inline markdown into (text, style) spans. Unterminated markers are
/// shown literally so a half-streamed `**bold` does not flicker.
pub fn inline_spans(text: &str) -> Vec<(String, InlineStyle)> {
    let mut spans: Vec<(String, InlineStyle)> = Vec::new();
    let style = InlineStyle::default();
    let mut current = String::new();
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;

    let push =
        |current: &mut String, style: InlineStyle, spans: &mut Vec<(String, InlineStyle)>| {
            if !current.is_empty() {
                spans.push((std::mem::take(current), style));
            }
        };

    while i < chars.len() {
        let c = chars[i];
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
                ));
                i = end + 1;
                continue;
            }
        }
        if c == '*' && i + 1 < chars.len() && chars[i + 1] == '*' {
            if let Some(end) = find_pair(&chars, i + 2) {
                push(&mut current, style, &mut spans);
                let inner: String = chars[i + 2..end].iter().collect();
                for (t, s) in inline_spans(&inner) {
                    spans.push((
                        t,
                        InlineStyle {
                            bold: true,
                            ..merge(style, s)
                        },
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
    }
}

fn find_from(chars: &[char], from: usize, needles: &[char]) -> Option<usize> {
    (from..chars.len()).find(|&i| needles.contains(&chars[i]))
}

fn find_pair(chars: &[char], from: usize) -> Option<usize> {
    (from..chars.len().saturating_sub(1)).find(|&i| chars[i] == '*' && chars[i + 1] == '*')
}
