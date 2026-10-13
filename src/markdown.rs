//! Discord's text formatting, as HTML safe to drop into a page.

pub fn render(src: &str) -> String {
    let lines: Vec<&str> = src.split('\n').collect();
    let mut doc = Doc::default();
    let mut i = 0;
    let mut fence: Option<String> = None;
    while i < lines.len() {
        let line = lines[i];
        if let Some(code) = fence.as_mut() {
            if line.trim() == "```" {
                doc.flush();
                doc.html.push_str("<pre><code>");
                doc.html.push_str(&escape(code));
                doc.html.push_str("</code></pre>");
                fence = None;
            } else {
                if !code.is_empty() {
                    code.push('\n');
                }
                code.push_str(line);
            }
            i += 1;
            continue;
        }
        if is_fence_open(line) {
            fence = Some(String::new());
            i += 1;
            continue;
        }
        if let Some(code) = one_line_fence(line) {
            doc.flush();
            doc.html.push_str("<pre><code>");
            doc.html.push_str(&escape(code));
            doc.html.push_str("</code></pre>");
            i += 1;
            continue;
        }
        if let Some(rest) = line.strip_prefix(">>>") {
            doc.flush();
            let mut quoted = Vec::new();
            let first = rest.trim_start();
            if !first.is_empty() {
                quoted.push(inline(first));
            }
            i += 1;
            while i < lines.len() {
                quoted.push(inline(lines[i]));
                i += 1;
            }
            push_quote(&mut doc.html, &quoted);
            break;
        }
        if line.trim().is_empty() {
            doc.flush();
            i += 1;
            continue;
        }
        if let Some((level, text)) = heading(line) {
            doc.flush();
            doc.html
                .push_str(&format!("<h{level}>{}</h{level}>", inline(text)));
            i += 1;
            continue;
        }
        if let Some(text) = quote_line(line) {
            doc.flush_para();
            if !matches!(doc.block, Block::Quote(_)) {
                doc.flush_block();
                doc.block = Block::Quote(Vec::new());
            }
            if let Block::Quote(lines) = &mut doc.block {
                lines.push(inline(text));
            }
            i += 1;
            continue;
        }
        if let Some(text) = bullet(line) {
            doc.flush_para();
            if !matches!(doc.block, Block::Ul(_)) {
                doc.flush_block();
                doc.block = Block::Ul(Vec::new());
            }
            if let Block::Ul(items) = &mut doc.block {
                items.push(inline(text));
            }
            i += 1;
            continue;
        }
        if let Some(text) = numbered(line) {
            doc.flush_para();
            if !matches!(doc.block, Block::Ol(_)) {
                doc.flush_block();
                doc.block = Block::Ol(Vec::new());
            }
            if let Block::Ol(items) = &mut doc.block {
                items.push(inline(text));
            }
            i += 1;
            continue;
        }
        doc.flush_block();
        doc.para.push(inline(line));
        i += 1;
    }
    if let Some(code) = fence {
        doc.flush();
        doc.html.push_str("<pre><code>");
        doc.html.push_str(&escape(&code));
        doc.html.push_str("</code></pre>");
    }
    doc.flush();
    doc.html
}

#[derive(Default)]
struct Doc {
    html: String,
    para: Vec<String>,
    block: Block,
}

#[derive(Default)]
enum Block {
    #[default]
    None,
    Quote(Vec<String>),
    Ul(Vec<String>),
    Ol(Vec<String>),
}

impl Doc {
    fn flush(&mut self) {
        self.flush_para();
        self.flush_block();
    }

    fn flush_para(&mut self) {
        if self.para.is_empty() {
            return;
        }
        self.html.push_str("<p>");
        self.html.push_str(&self.para.join("<br>"));
        self.html.push_str("</p>");
        self.para.clear();
    }

    fn flush_block(&mut self) {
        match std::mem::take(&mut self.block) {
            Block::None => {}
            Block::Quote(lines) => push_quote(&mut self.html, &lines),
            Block::Ul(items) => push_list(&mut self.html, "ul", &items),
            Block::Ol(items) => push_list(&mut self.html, "ol", &items),
        }
    }
}

fn push_quote(html: &mut String, lines: &[String]) {
    html.push_str("<blockquote><p>");
    html.push_str(&lines.join("<br>"));
    html.push_str("</p></blockquote>");
}

fn push_list(html: &mut String, tag: &str, items: &[String]) {
    html.push('<');
    html.push_str(tag);
    html.push('>');
    for item in items {
        html.push_str("<li>");
        html.push_str(item);
        html.push_str("</li>");
    }
    html.push_str("</");
    html.push_str(tag);
    html.push('>');
}

fn is_fence_open(line: &str) -> bool {
    let Some(rest) = line.strip_prefix("```") else {
        return false;
    };
    rest.chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '-' || c == '_')
}

fn one_line_fence(line: &str) -> Option<&str> {
    let rest = line.strip_prefix("```")?;
    let code = rest.strip_suffix("```")?;
    if code.is_empty() || code.contains("```") {
        None
    } else {
        Some(code)
    }
}

fn heading(line: &str) -> Option<(u8, &str)> {
    let hashes = line.chars().take_while(|c| *c == '#').count();
    if !(1..=3).contains(&hashes) {
        return None;
    }
    let rest = line.get(hashes..)?;
    let text = rest.strip_prefix(' ')?;
    Some((hashes as u8, text))
}

fn quote_line(line: &str) -> Option<&str> {
    if line.starts_with(">>>") {
        return None;
    }
    let rest = line.strip_prefix('>')?;
    Some(rest.strip_prefix(' ').unwrap_or(rest))
}

fn bullet(line: &str) -> Option<&str> {
    line.strip_prefix("- ").or_else(|| line.strip_prefix("* "))
}

fn numbered(line: &str) -> Option<&str> {
    let bytes = line.as_bytes();
    let mut i = 0;
    if bytes.first()?.is_ascii_digit() {
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
        }
    } else {
        return None;
    }
    if bytes.get(i) == Some(&b'.') && bytes.get(i + 1) == Some(&b' ') {
        Some(&line[i + 2..])
    } else {
        None
    }
}

fn inline(src: &str) -> String {
    let chars: Vec<char> = src.chars().collect();
    inline_chars(&chars)
}

fn inline_chars(chars: &[char]) -> String {
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '\\' && chars.get(i + 1).is_some_and(|c| is_marker(*c)) {
            out.push_str(&escape_char(chars[i + 1]));
            i += 2;
            continue;
        }
        if let Some((html, used)) = take_code(&chars[i..]) {
            out.push_str(&html);
            i += used;
            continue;
        }
        if let Some((html, used)) = take_link(&chars[i..]) {
            out.push_str(&html);
            i += used;
            continue;
        }
        if let Some((html, used)) = take_wrapped(&chars[i..], i == 0 || !is_word(chars[i - 1])) {
            out.push_str(&html);
            i += used;
            continue;
        }
        out.push_str(&escape_char(chars[i]));
        i += 1;
    }
    out
}

fn take_code(chars: &[char]) -> Option<(String, usize)> {
    if chars.first() != Some(&'`') || chars.get(1) == Some(&'`') {
        return None;
    }
    let end = chars.iter().skip(1).position(|c| *c == '`')? + 1;
    if end == 1 {
        return None;
    }
    let content: String = chars[1..end].iter().collect();
    Some((format!("<code>{}</code>", escape(&content)), end + 1))
}

fn take_link(chars: &[char]) -> Option<(String, usize)> {
    if chars.first() != Some(&'[') {
        return None;
    }
    let label_end = chars.iter().skip(1).position(|c| *c == ']')? + 1;
    if label_end == 1 || chars.get(label_end + 1) != Some(&'(') {
        return None;
    }
    let url_start = label_end + 2;
    let url_end = chars.iter().skip(url_start).position(|c| *c == ')')? + url_start;
    let url: String = chars[url_start..url_end].iter().collect();
    if !safe_url(&url) {
        return None;
    }
    let label = inline_chars(&chars[1..label_end]);
    Some((
        format!("<a href=\"{}\">{label}</a>", escape(&url)),
        url_end + 1,
    ))
}

fn safe_url(url: &str) -> bool {
    (url.starts_with("https://") || url.starts_with("http://"))
        && url
            .chars()
            .all(|c| !c.is_whitespace() && c != '"' && c != '<' && c != '>')
}

fn take_wrapped(chars: &[char], underscore_ok: bool) -> Option<(String, usize)> {
    let patterns: &[(&str, &str, bool)] = &[
        ("***", "<strong><em>", false),
        ("**", "<strong>", false),
        ("__", "<u>", true),
        ("~~", "<s>", false),
        ("||", "<span class=\"spoiler\">", false),
        ("*", "<em>", false),
        ("_", "<em>", true),
    ];
    for (delim, open, needs_edge) in patterns {
        let marker: Vec<char> = delim.chars().collect();
        if !chars.starts_with(&marker) {
            continue;
        }
        if *needs_edge && !underscore_ok {
            continue;
        }
        let close = find_close(chars, marker.len(), &marker, *needs_edge)?;
        if close == marker.len() {
            continue;
        }
        let inner = inline_chars(&chars[marker.len()..close]);
        let close_tag = match *delim {
            "***" => "</em></strong>",
            "**" => "</strong>",
            "__" => "</u>",
            "~~" => "</s>",
            "||" => "</span>",
            "*" | "_" => "</em>",
            _ => "",
        };
        return Some((format!("{open}{inner}{close_tag}"), close + marker.len()));
    }
    None
}

fn find_close(chars: &[char], start: usize, delim: &[char], needs_edge: bool) -> Option<usize> {
    let mut i = start;
    while i + delim.len() <= chars.len() {
        if chars[i] == '\\' && i + 1 < chars.len() {
            i += 2;
            continue;
        }
        if chars[i..].starts_with(delim) {
            let after = i + delim.len();
            if !needs_edge || after == chars.len() || !is_word(chars[after]) {
                return Some(i);
            }
        }
        i += 1;
    }
    None
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn is_marker(c: char) -> bool {
    matches!(c, '\\' | '*' | '_' | '~' | '|' | '`' | '[' | ']')
}

fn escape(src: &str) -> String {
    src.chars().map(|c| escape_char(c)).collect()
}

fn escape_char(c: char) -> String {
    match c {
        '&' => "&amp;".into(),
        '<' => "&lt;".into(),
        '>' => "&gt;".into(),
        '"' => "&quot;".into(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formatting_matches_discord() {
        assert_eq!(render("**bold**"), "<p><strong>bold</strong></p>");
        assert_eq!(render("*italic*"), "<p><em>italic</em></p>");
        assert_eq!(render("__under__"), "<p><u>under</u></p>");
        assert_eq!(render("snake_case"), "<p>snake_case</p>");
        assert_eq!(render("~~gone~~"), "<p><s>gone</s></p>");
        assert_eq!(
            render("**a *b* c**"),
            "<p><strong>a <em>b</em> c</strong></p>"
        );
        assert_eq!(render("`a<b>`"), "<p><code>a&lt;b&gt;</code></p>");
        assert_eq!(
            render("[hall](https://example.com/a)"),
            "<p><a href=\"https://example.com/a\">hall</a></p>"
        );
        assert_eq!(
            render("[x](javascript:alert(1))"),
            "<p>[x](javascript:alert(1))</p>"
        );
        assert_eq!(render("<script>"), "<p>&lt;script&gt;</p>");
        assert_eq!(
            render("||secret||"),
            "<p><span class=\"spoiler\">secret</span></p>"
        );
        assert_eq!(render("> noted"), "<blockquote><p>noted</p></blockquote>");
        assert_eq!(render("# Title"), "<h1>Title</h1>");
        assert_eq!(render("- one\n- two"), "<ul><li>one</li><li>two</li></ul>");
        let fenced = render("```\n**no**\n```");
        assert!(
            fenced.contains("<pre><code>**no**</code></pre>"),
            "{fenced}"
        );
        assert!(!fenced.contains("<strong>"), "{fenced}");
    }
}
