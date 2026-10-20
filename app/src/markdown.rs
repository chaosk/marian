//! Discord's text formatting, as HTML safe to drop into a page.
//!
//! [`discord_md::parse`](https://docs.rs/discord-md/latest/discord_md/) owns the
//! Discord markers. This walks that AST and escapes the text.

use discord_md::ast::{MarkdownElement, MarkdownElementCollection};
use discord_md::parse;

pub fn render(src: &str) -> String {
    let mut html = String::new();
    blocks(&mut html, parse(src).content());
    html
}

fn blocks(html: &mut String, elements: &MarkdownElementCollection) {
    let mut inline = String::new();
    for element in elements.get() {
        match element {
            MarkdownElement::MultiLineCode(code) => {
                flush_inline(html, &mut inline);
                html.push_str("<pre><code>");
                html.push_str(&escape(code.content().trim_matches('\n')));
                html.push_str("</code></pre>");
            }
            MarkdownElement::BlockQuote(quote) => {
                flush_inline(html, &mut inline);
                html.push_str("<blockquote>");
                blocks(html, quote.content());
                html.push_str("</blockquote>");
            }
            other => inline.push_str(&span(other)),
        }
    }
    flush_inline(html, &mut inline);
}

fn span(element: &MarkdownElement) -> String {
    match element {
        MarkdownElement::Plain(text) => plain(text.content()),
        MarkdownElement::ItalicsStar(inner) => wrap("em", inner.content()),
        MarkdownElement::ItalicsUnderscore(inner) => wrap("em", inner.content()),
        MarkdownElement::Bold(inner) => wrap("strong", inner.content()),
        MarkdownElement::Underline(inner) => wrap("u", inner.content()),
        MarkdownElement::Strikethrough(inner) => wrap("s", inner.content()),
        MarkdownElement::Spoiler(inner) => {
            format!(
                "<span class=\"spoiler\">{}</span>",
                spans(inner.content())
            )
        }
        MarkdownElement::OneLineCode(code) => format!("<code>{}</code>", escape(code.content())),
        MarkdownElement::MultiLineCode(_) | MarkdownElement::BlockQuote(_) => String::new(),
    }
}

fn wrap(tag: &str, content: &MarkdownElementCollection) -> String {
    format!("<{tag}>{}</{tag}>", spans(content))
}

fn spans(elements: &MarkdownElementCollection) -> String {
    elements.get().iter().map(span).collect()
}

fn plain(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(open) = rest.find('[') {
        out.push_str(&escape_keep_nl(&rest[..open]));
        match take_link(&rest[open..]) {
            Some((html, used)) => {
                out.push_str(&html);
                rest = &rest[open + used..];
            }
            None => {
                out.push('[');
                rest = &rest[open + 1..];
            }
        }
    }
    out.push_str(&escape_keep_nl(rest));
    out
}

fn take_link(src: &str) -> Option<(String, usize)> {
    let label_end = src.find(']')?;
    if label_end <= 1 || !src[label_end + 1..].starts_with('(') {
        return None;
    }
    let url_start = label_end + 2;
    let url_end = src[url_start..].find(')')? + url_start;
    let url = &src[url_start..url_end];
    if !safe_url(url) {
        return None;
    }
    let label = plain(&src[1..label_end]);
    Some((
        format!("<a href=\"{}\">{label}</a>", escape(url)),
        url_end + 1,
    ))
}

fn safe_url(url: &str) -> bool {
    (url.starts_with("https://") || url.starts_with("http://"))
        && url
            .chars()
            .all(|c| !c.is_whitespace() && c != '"' && c != '<' && c != '>')
}

fn flush_inline(html: &mut String, inline: &mut String) {
    if inline.is_empty() {
        return;
    }
    let text = std::mem::take(inline);
    let lines: Vec<&str> = text.split('\n').collect();
    let mut para: Vec<&str> = Vec::new();
    let mut ul: Vec<&str> = Vec::new();
    let mut ol: Vec<&str> = Vec::new();
    for line in lines {
        if line.is_empty() {
            flush_para(html, &mut para);
            flush_list(html, "ul", &mut ul);
            flush_list(html, "ol", &mut ol);
            continue;
        }
        if let Some((level, title)) = heading(line) {
            flush_para(html, &mut para);
            flush_list(html, "ul", &mut ul);
            flush_list(html, "ol", &mut ol);
            html.push_str(&format!("<h{level}>{title}</h{level}>"));
            continue;
        }
        if let Some(item) = line.strip_prefix("- ").or_else(|| line.strip_prefix("* ")) {
            flush_para(html, &mut para);
            flush_list(html, "ol", &mut ol);
            ul.push(item);
            continue;
        }
        if let Some(item) = numbered(line) {
            flush_para(html, &mut para);
            flush_list(html, "ul", &mut ul);
            ol.push(item);
            continue;
        }
        if let Some(quoted) = quote_line(line) {
            flush_para(html, &mut para);
            flush_list(html, "ul", &mut ul);
            flush_list(html, "ol", &mut ol);
            html.push_str("<blockquote><p>");
            html.push_str(quoted);
            html.push_str("</p></blockquote>");
            continue;
        }
        flush_list(html, "ul", &mut ul);
        flush_list(html, "ol", &mut ol);
        para.push(line);
    }
    flush_para(html, &mut para);
    flush_list(html, "ul", &mut ul);
    flush_list(html, "ol", &mut ol);
}

fn heading(line: &str) -> Option<(u8, &str)> {
    let hashes = line.chars().take_while(|c| *c == '#').count();
    if !(1..=3).contains(&hashes) {
        return None;
    }
    let rest = line.get(hashes..)?;
    Some((hashes as u8, rest.strip_prefix(' ')?))
}

fn numbered(line: &str) -> Option<&str> {
    let digits = line.bytes().take_while(u8::is_ascii_digit).count();
    if digits == 0 {
        return None;
    }
    line.get(digits..)?.strip_prefix(". ")
}

fn quote_line(line: &str) -> Option<&str> {
    if line.starts_with(">>>") {
        return None;
    }
    let rest = line.strip_prefix("&gt;").or_else(|| line.strip_prefix('>'))?;
    Some(rest.strip_prefix(' ').unwrap_or(rest))
}

fn flush_para(html: &mut String, para: &mut Vec<&str>) {
    if para.is_empty() {
        return;
    }
    html.push_str("<p>");
    html.push_str(&para.join("<br>"));
    html.push_str("</p>");
    para.clear();
}

fn flush_list(html: &mut String, tag: &str, items: &mut Vec<&str>) {
    if items.is_empty() {
        return;
    }
    html.push('<');
    html.push_str(tag);
    html.push('>');
    for item in items.iter() {
        html.push_str("<li>");
        html.push_str(item);
        html.push_str("</li>");
    }
    html.push_str("</");
    html.push_str(tag);
    html.push('>');
    items.clear();
}

fn escape_keep_nl(src: &str) -> String {
    src.chars()
        .map(|c| if c == '\n' { c.to_string() } else { escape_char(c) })
        .collect()
}

fn escape(src: &str) -> String {
    src.chars().map(escape_char).collect()
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
