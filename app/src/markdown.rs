//! Discord's text formatting, as HTML safe to drop into a page.
//!
//! [`discord_md::parse`](https://docs.rs/discord-md/latest/discord_md/) owns the
//! Discord markers. This walks that AST and escapes the text.

use discord_md::ast::{MarkdownElement, MarkdownElementCollection};
use discord_md::parse;

pub fn render(src: &str) -> String {
    let (src, urls) = shield(src);
    let mut html = String::new();
    blocks(&mut html, parse(&src).content());
    unshield(&html, &urls)
}

/// Marks a web address set aside by [`shield`]: its index between the two.
const URL_OPEN: char = '\u{E000}';
const URL_CLOSE: char = '\u{E001}';

/// Swaps each web address for a token the parser leaves alone, so the `_`, `*`
/// or `~~` in one stays part of it rather than starting formatting, as in
/// Discord. [`unshield`] puts them back once the HTML is built.
fn shield(src: &str) -> (String, Vec<String>) {
    let mut out = String::with_capacity(src.len());
    let mut urls = Vec::new();
    let mut rest = src;
    while let Some(start) = find_url(rest) {
        let (before, from) = rest.split_at(start);
        out.push_str(before);
        let len = url_len(from, before.ends_with("]("));
        out.push(URL_OPEN);
        out.push_str(&urls.len().to_string());
        out.push(URL_CLOSE);
        urls.push(from[..len].to_string());
        rest = &from[len..];
    }
    out.push_str(rest);
    (out, urls)
}

fn find_url(src: &str) -> Option<usize> {
    src.match_indices("http")
        .map(|(at, _)| at)
        .find(|&at| src[at..].starts_with("https://") || src[at..].starts_with("http://"))
}

/// Where the address at the start of `src` ends: at the `)` closing a link's
/// target, or for a bare one at a space or the `](` after a link's label, less
/// any punctuation or formatting markers trailing it.
fn url_len(src: &str, in_link: bool) -> usize {
    let end = src
        .find(|c: char| c.is_whitespace() || matches!(c, '"' | '<' | '>') || (in_link && c == ')'))
        .unwrap_or(src.len());
    let url = &src[..end];
    if in_link {
        return url.len();
    }
    let url = url.find("](").map_or(url, |label_end| &url[..label_end]);
    url.trim_end_matches([
        '*', '_', '~', '|', '`', '.', ',', ';', ':', '!', '?', ')', '\'',
    ])
    .len()
}

fn shielded(url: &str) -> bool {
    url.strip_prefix(URL_OPEN)
        .and_then(|rest| rest.strip_suffix(URL_CLOSE))
        .is_some_and(|index| !index.is_empty() && index.bytes().all(|b| b.is_ascii_digit()))
}

fn unshield(html: &str, urls: &[String]) -> String {
    let mut out = String::with_capacity(html.len());
    let mut rest = html;
    while let Some(open) = rest.find(URL_OPEN) {
        out.push_str(&rest[..open]);
        let after = &rest[open + URL_OPEN.len_utf8()..];
        let url = after.find(URL_CLOSE).and_then(|close| {
            let url = urls.get(after[..close].parse::<usize>().ok()?)?;
            Some((url, close))
        });
        match url {
            Some((url, close)) => {
                out.push_str(&escape(url));
                rest = &after[close + URL_CLOSE.len_utf8()..];
            }
            None => {
                out.push(URL_OPEN);
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
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
        MarkdownElement::Plain(text) => plain(text.content(), true),
        MarkdownElement::ItalicsStar(inner) => wrap("em", inner.content()),
        MarkdownElement::ItalicsUnderscore(inner) => wrap("em", inner.content()),
        MarkdownElement::Bold(inner) => wrap("strong", inner.content()),
        MarkdownElement::Underline(inner) => wrap("u", inner.content()),
        MarkdownElement::Strikethrough(inner) => wrap("s", inner.content()),
        // Hidden until clicked, or focused and Enter or Space pressed: see
        // `reveal_spoilers`. A span, not a button, since a link may sit inside.
        MarkdownElement::Spoiler(inner) => {
            format!(
                "<span class=\"spoiler\" role=\"button\" tabindex=\"0\" aria-expanded=\"false\">{}</span>",
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

/// Text with its `[label](address)` links, and with `bare` its bare addresses
/// linked too (not in a link's label, since links don't nest).
fn plain(text: &str, bare: bool) -> String {
    let words = |text: &str| {
        if bare {
            link_bare(text)
        } else {
            escape_keep_nl(text)
        }
    };
    let mut out = String::new();
    let mut rest = text;
    while let Some(open) = rest.find('[') {
        out.push_str(&words(&rest[..open]));
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
    out.push_str(&words(rest));
    out
}

/// Each address [`shield`] set aside in `text` as a link to itself. Wrapped in
/// `<…>`, as Discord writes one with its preview turned off, it loses those.
fn link_bare(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(open) = rest.find(URL_OPEN) {
        let token = rest[open..]
            .find(URL_CLOSE)
            .map(|close| &rest[open..open + close + URL_CLOSE.len_utf8()])
            .filter(|token| shielded(token));
        let Some(token) = token else {
            let next = open + URL_OPEN.len_utf8();
            out.push_str(&escape_keep_nl(&rest[..next]));
            rest = &rest[next..];
            continue;
        };
        let mut before = &rest[..open];
        let mut after = &rest[open + token.len()..];
        if let (Some(b), Some(a)) = (before.strip_suffix('<'), after.strip_prefix('>')) {
            before = b;
            after = a;
        }
        out.push_str(&escape_keep_nl(before));
        out.push_str(&format!("<a href=\"{token}\">{token}</a>"));
        rest = after;
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
    if !(shielded(url) || safe_url(url)) {
        return None;
    }
    let label = plain(&src[1..label_end], false);
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
    let rest = line
        .strip_prefix("&gt;")
        .or_else(|| line.strip_prefix('>'))?;
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
        .map(|c| {
            if c == '\n' {
                c.to_string()
            } else {
                escape_char(c)
            }
        })
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

/// Spoilers anywhere on the page open on a click, or on Enter or Space while
/// focused, and close the same way. One listener on the document for them
/// all, since they arrive as HTML from `render` rather than as views. A click
/// on a link inside a closed spoiler only opens it, as in Discord.
#[cfg(feature = "hydrate")]
pub fn reveal_spoilers() {
    use wasm_bindgen::JsCast;
    use wasm_bindgen::closure::Closure;
    thread_local! {
        static LISTENING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }
    if LISTENING.with(|on| on.replace(true)) {
        return;
    }
    let toggle = |ev: &web_sys::Event| {
        let Some(spoiler) = ev
            .target()
            .and_then(|target| target.dyn_into::<web_sys::Element>().ok())
            .and_then(|element| element.closest(".md .spoiler").ok().flatten())
        else {
            return;
        };
        let open = spoiler.class_list().toggle("open").unwrap_or(false);
        let _ = spoiler.set_attribute("aria-expanded", if open { "true" } else { "false" });
        // Opening is all a click on a hidden spoiler does: not following a link
        // inside it, and not opening the editor of the description around it.
        ev.prevent_default();
        ev.stop_propagation();
    };
    let on_click = Closure::<dyn FnMut(web_sys::Event)>::new(move |ev: web_sys::Event| {
        let opened_link = ev
            .target()
            .and_then(|target| target.dyn_into::<web_sys::Element>().ok())
            .and_then(|element| element.closest(".md .spoiler.open a").ok().flatten())
            .is_some();
        if !opened_link {
            toggle(&ev);
        }
    })
    .into_js_value();
    let on_key =
        Closure::<dyn FnMut(web_sys::KeyboardEvent)>::new(move |ev: web_sys::KeyboardEvent| {
            let on_spoiler = ev
                .target()
                .and_then(|target| target.dyn_into::<web_sys::Element>().ok())
                .is_some_and(|element| element.matches(".md .spoiler").unwrap_or(false));
            if on_spoiler && matches!(ev.key().as_str(), "Enter" | " ") {
                toggle(&ev);
            }
        })
        .into_js_value();
    let document = leptos::prelude::document();
    let _ =
        document.add_event_listener_with_callback_and_bool("click", on_click.unchecked_ref(), true);
    let _ =
        document.add_event_listener_with_callback_and_bool("keydown", on_key.unchecked_ref(), true);
}

#[cfg(not(feature = "hydrate"))]
pub fn reveal_spoilers() {}

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
            render(
                "[Goose Goose Duck](https://store.steampowered.com/app/1568590/Goose_Goose_Duck/)"
            ),
            "<p><a href=\"https://store.steampowered.com/app/1568590/Goose_Goose_Duck/\">Goose Goose Duck</a></p>",
            "a link's underscores are part of it"
        );
        assert_eq!(
            render("see https://example.com/a_b_c and _this_"),
            "<p>see <a href=\"https://example.com/a_b_c\">https://example.com/a_b_c</a> and <em>this</em></p>"
        );
        assert_eq!(
            render("**https://example.com/a_b**"),
            "<p><strong><a href=\"https://example.com/a_b\">https://example.com/a_b</a></strong></p>"
        );
        assert_eq!(
            render("no preview: <https://example.com/a>."),
            "<p>no preview: <a href=\"https://example.com/a\">https://example.com/a</a>.</p>"
        );
        assert_eq!(
            render("(https://example.com/a), <b>"),
            "<p>(<a href=\"https://example.com/a\">https://example.com/a</a>), &lt;b&gt;</p>"
        );
        assert_eq!(
            render("[https://example.com/a](https://example.com/b)"),
            "<p><a href=\"https://example.com/b\">https://example.com/a</a></p>",
            "links don't nest"
        );
        assert_eq!(
            render("||https://example.com/a||"),
            "<p><span class=\"spoiler\" role=\"button\" tabindex=\"0\" aria-expanded=\"false\"><a href=\"https://example.com/a\">https://example.com/a</a></span></p>"
        );
        assert_eq!(
            render("`https://example.com/a_b_c`"),
            "<p><code>https://example.com/a_b_c</code></p>"
        );
        assert_eq!(
            render("[x](https://example.com/\"onmouseover)"),
            "<p>[x](<a href=\"https://example.com/\">https://example.com/</a>&quot;onmouseover)</p>",
            "a quote ends an address, outside the link"
        );
        assert_eq!(
            render("[x](javascript:alert(1))"),
            "<p>[x](javascript:alert(1))</p>"
        );
        assert_eq!(render("<script>"), "<p>&lt;script&gt;</p>");
        assert_eq!(
            render("||secret||"),
            "<p><span class=\"spoiler\" role=\"button\" tabindex=\"0\" aria-expanded=\"false\">secret</span></p>"
        );
        assert_eq!(
            render("||<b>&||"),
            "<p><span class=\"spoiler\" role=\"button\" tabindex=\"0\" aria-expanded=\"false\">&lt;b&gt;&amp;</span></p>",
            "a spoiler's text stays escaped"
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
