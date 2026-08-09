//! HTML → Markdown engine. Rust equivalent of src/utils/turndown.ts.
//!
//! Uses scraper (html5ever) to parse HTML and walks the DOM to produce GFM
//! markdown: atx headings, fenced code blocks, "-" bullets, ordered lists
//! (respecting start), GFM tables, ~~strikethrough~~, **bold**, _italic_,
//! [links](href), ![images](src), blockquotes, horizontal rules, etc.

use regex::Regex;
use scraper::{ElementRef, Html, Node};

// ============================== Public API ==============================

/// Convert an HTML fragment or document to GFM markdown.
pub fn html_to_markdown(html: &str) -> String {
    let doc = Html::parse_fragment(html);
    let root = doc.root_element();
    let mut ctx = Ctx::default();
    walk_children(root, &mut ctx);
    collapse_blank_lines(ctx.out.trim().to_string())
}

/// Normalize HTML tables so the table converter can handle them:
/// - Strip <p> tags inside <td>/<th> cells (join multiple paragraphs with space)
/// - Promote first row to <thead>/<th> when <thead> is missing
pub fn normalize_tables_html(html: &str) -> String {
    // 1. Strip <p> inside cells
    let re_cell = Regex::new(r"(?is)<(td|th)([^>]*)>([\s\S]*?)</(td|th)>").unwrap();
    let step1 = re_cell.replace_all(html, |caps: &regex::Captures| {
        let tag = &caps[1];
        let attrs = &caps[2];
        let inner = &caps[3];
        let close = &caps[4];
        let stripped = strip_p_in_cell(inner);
        format!("<{tag}{attrs}>{stripped}</{close}>")
    });

    // 2. Add <thead> to tables that lack it
    let re_table = Regex::new(
        r"(?is)<table([^>]*)>\s*(?:<tbody>\s*)?(<tr[\s\S]*?</tr>)([\s\S]*?)</(?:tbody>\s*</)?table>",
    )
    .unwrap();
    let step2 = re_table.replace_all(&step1, |caps: &regex::Captures| {
        let attrs = &caps[1];
        let first_row = &caps[2];
        let rest = &caps[3];
        let re_td_open = Regex::new(r"(?i)<td").unwrap();
        let re_td_close = Regex::new(r"(?i)</td>").unwrap();
        let thead_row = re_td_open.replace_all(first_row, "<th");
        let thead_row = re_td_close.replace_all(&thead_row, "</th>");
        format!("<table{attrs}><thead>{thead_row}</thead><tbody>{rest}</tbody></table>")
    });

    step2.into_owned()
}

// ============================== Internal types ==============================

#[derive(Default)]
struct Ctx {
    out: String,
    list_stack: Vec<ListCtx>,
    in_pre: bool,
    in_table: bool,
    table_rows: Vec<Vec<String>>,
    table_cells: Vec<String>,
    in_cell: bool,
    cell_buf: String,
    in_heading: bool,
}

struct ListCtx {
    ordered: bool,
    start: usize,
    item_index: usize,
}

// ============================== DOM walker ==============================

fn walk_children(el: ElementRef, ctx: &mut Ctx) {
    for child in el.children() {
        match child.value() {
            Node::Text(text) => handle_text(&text.text, ctx),
            Node::Element(_) => {
                if let Some(child_el) = ElementRef::wrap(child) {
                    handle_element(child_el, ctx);
                }
            }
            _ => {}
        }
    }
}

fn handle_text(text: &str, ctx: &mut Ctx) {
    if ctx.in_table && !ctx.in_cell {
        return;
    }
    if ctx.in_cell {
        if ctx.in_pre {
            ctx.cell_buf.push_str(text);
        } else {
            ctx.cell_buf.push_str(&collapse_whitespace(text));
        }
        return;
    }
    if ctx.in_pre {
        ctx.out.push_str(text);
        return;
    }
    let collapsed = collapse_whitespace(text);
    if collapsed.is_empty() {
        return;
    }
    let escaped = escape_markdown(&collapsed);
    ctx.out.push_str(&escaped);
}

fn handle_element(el: ElementRef, ctx: &mut Ctx) {
    let tag = el.value().name().to_lowercase();
    match tag.as_str() {
        "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => handle_heading(el, ctx, &tag),
        "p" => handle_paragraph(el, ctx),
        "br" => handle_br(ctx),
        "hr" => handle_hr(ctx),
        "strong" | "b" => handle_wrap(el, ctx, "**"),
        "em" | "i" => handle_wrap(el, ctx, "_"),
        "del" | "s" | "strike" => handle_wrap(el, ctx, "~~"),
        "a" => handle_link(el, ctx),
        "img" => handle_img(el, ctx),
        "code" => handle_code(el, ctx),
        "pre" => handle_pre(el, ctx),
        "blockquote" => handle_blockquote(el, ctx),
        "ul" => handle_list(el, ctx, false),
        "ol" => handle_list(el, ctx, true),
        "li" => handle_li(el, ctx),
        "table" => handle_table(el, ctx),
        "thead" | "tbody" | "tfoot" => walk_children(el, ctx),
        "tr" => handle_tr(el, ctx),
        "td" | "th" => handle_td(el, ctx),
        // turndown keeps <title> text (default rule) — replicate that quirk
        "script" | "style" => {}
        _ => walk_children(el, ctx),
    }
}

// ============================== Element handlers ==============================

fn handle_heading(el: ElementRef, ctx: &mut Ctx, tag: &str) {
    let level: usize = tag[1..].parse().unwrap_or(1);
    let prefix = "#".repeat(level);

    let was = ctx.in_heading;
    ctx.in_heading = true;
    let content = inner_markdown(el, ctx);
    ctx.in_heading = was;

    // Unescape "\." in heading text (custom rule from turndown.ts)
    let cleaned = content.replace(r"\.", ".").trim().to_string();

    ensure_blank_line(&mut ctx.out);
    ctx.out.push_str(&prefix);
    ctx.out.push(' ');
    ctx.out.push_str(&cleaned);
    ctx.out.push_str("\n\n");
}

fn handle_paragraph(el: ElementRef, ctx: &mut Ctx) {
    if ctx.in_cell {
        walk_children(el, ctx);
        return;
    }
    ensure_blank_line(&mut ctx.out);
    walk_children(el, ctx);
    ctx.out.push_str("\n\n");
}

fn handle_br(ctx: &mut Ctx) {
    if ctx.in_cell {
        ctx.cell_buf.push_str("  \n");
    } else {
        ctx.out.push_str("  \n");
    }
}

fn handle_hr(ctx: &mut Ctx) {
    ensure_blank_line(&mut ctx.out);
    ctx.out.push_str("* * *\n\n");
}

fn handle_wrap(el: ElementRef, ctx: &mut Ctx, marker: &str) {
    if ctx.in_cell {
        let content = inner_markdown(el, ctx);
        if !content.is_empty() {
            ctx.cell_buf.push_str(marker);
            ctx.cell_buf.push_str(&content);
            ctx.cell_buf.push_str(marker);
        }
        return;
    }
    let content = inner_markdown(el, ctx);
    if content.is_empty() {
        return;
    }
    ctx.out.push_str(marker);
    ctx.out.push_str(&content);
    ctx.out.push_str(marker);
}

fn handle_link(el: ElementRef, ctx: &mut Ctx) {
    let href = el.value().attr("href").unwrap_or("");
    let content = inner_markdown(el, ctx);
    let result = format!("[{content}]({href})");
    if ctx.in_cell {
        ctx.cell_buf.push_str(&result);
    } else {
        ctx.out.push_str(&result);
    }
}

fn handle_img(el: ElementRef, ctx: &mut Ctx) {
    let alt = el.value().attr("alt").unwrap_or("");
    let src = el.value().attr("src").unwrap_or("");
    let result = format!("![{alt}]({src})");
    if ctx.in_cell {
        ctx.cell_buf.push_str(&result);
    } else {
        ctx.out.push_str(&result);
    }
}

fn handle_code(el: ElementRef, ctx: &mut Ctx) {
    if ctx.in_pre {
        walk_children(el, ctx);
        return;
    }
    let text = el.text().collect::<String>();
    let result = format!("`{text}`");
    if ctx.in_cell {
        ctx.cell_buf.push_str(&result);
    } else {
        ctx.out.push_str(&result);
    }
}

fn handle_pre(el: ElementRef, ctx: &mut Ctx) {
    let code_child = el
        .children()
        .filter_map(|c| ElementRef::wrap(c))
        .find(|c| c.value().name() == "code");

    let lang = code_child.and_then(|code_el| {
        code_el.value().attr("class").and_then(|cls| {
            cls.split_whitespace()
                .find_map(|c| c.strip_prefix("language-").map(|l| l.to_string()))
        })
    });

    if code_child.is_some() {
        ensure_blank_line(&mut ctx.out);
        ctx.out.push_str("```");
        if let Some(ref l) = lang {
            ctx.out.push_str(l);
        }
        ctx.out.push('\n');
        ctx.in_pre = true;
        walk_children(el, ctx);
        ctx.in_pre = false;
        if !ctx.out.ends_with('\n') {
            ctx.out.push('\n');
        }
        ctx.out.push_str("```\n\n");
    } else {
        // <pre> without <code>
        ctx.in_pre = true;
        walk_children(el, ctx);
        ctx.in_pre = false;
        ctx.out.push_str("\n\n");
    }
}

fn handle_blockquote(el: ElementRef, ctx: &mut Ctx) {
    let content = inner_markdown(el, ctx);
    let content = content.trim();
    ensure_blank_line(&mut ctx.out);
    for line in content.split('\n') {
        if line.is_empty() {
            ctx.out.push_str(">\n");
        } else {
            ctx.out.push_str("> ");
            ctx.out.push_str(line);
            ctx.out.push('\n');
        }
    }
    ctx.out.push('\n');
}

fn handle_list(el: ElementRef, ctx: &mut Ctx, ordered: bool) {
    let start: usize = if ordered {
        el.value()
            .attr("start")
            .and_then(|s| s.parse().ok())
            .unwrap_or(1)
    } else {
        1
    };

    let is_top_level = ctx.list_stack.is_empty();
    if is_top_level {
        ensure_blank_line(&mut ctx.out);
    } else if !ctx.out.ends_with('\n') {
        ctx.out.push('\n');
    }

    ctx.list_stack.push(ListCtx {
        ordered,
        start,
        item_index: 0,
    });

    walk_children(el, ctx);

    ctx.list_stack.pop();

    if is_top_level {
        ctx.out.push('\n');
    }
}

fn handle_li(el: ElementRef, ctx: &mut Ctx) {
    let depth = ctx.list_stack.len();
    if depth == 0 {
        walk_children(el, ctx);
        return;
    }

    let list_ctx = ctx.list_stack.last_mut().unwrap();
    let prefix = if list_ctx.ordered {
        let num = list_ctx.start + list_ctx.item_index;
        format!("{num}. ")
    } else {
        "- ".to_string()
    };
    list_ctx.item_index += 1;

    let content = inner_markdown(el, ctx);
    let content = content
        .trim_start_matches('\n')
        .trim_end_matches('\n');

    // Indent continuation lines by 2 spaces (like turndown)
    let indented = content.replace('\n', "\n  ");

    ctx.out.push_str(&prefix);
    ctx.out.push_str(&indented);
    if el.next_sibling().is_some() {
        ctx.out.push('\n');
    }
}

// ============================== Table handling ==============================

fn handle_table(el: ElementRef, ctx: &mut Ctx) {
    let was_in_table = ctx.in_table;
    let saved_rows = std::mem::take(&mut ctx.table_rows);
    let saved_cells = std::mem::take(&mut ctx.table_cells);
    ctx.in_table = true;

    walk_children(el, ctx);

    ctx.in_table = was_in_table;

    if ctx.table_rows.is_empty() {
        ctx.table_rows = saved_rows;
        ctx.table_cells = saved_cells;
        return;
    }

    ensure_blank_line(&mut ctx.out);

    let header = &ctx.table_rows[0];
    let col_count = header.len();

    ctx.out.push_str("| ");
    ctx.out.push_str(&header.join(" | "));
    ctx.out.push_str(" |\n");

    ctx.out.push_str("| ");
    ctx.out.push_str(&vec!["---"; col_count].join(" | "));
    ctx.out.push_str(" |\n");

    for row in &ctx.table_rows[1..] {
        ctx.out.push_str("| ");
        let mut padded = row.clone();
        while padded.len() < col_count {
            padded.push(String::new());
        }
        ctx.out.push_str(&padded.join(" | "));
        ctx.out.push_str(" |\n");
    }

    ctx.out.push('\n');
    ctx.table_rows = saved_rows;
    ctx.table_cells = saved_cells;
}

fn handle_tr(el: ElementRef, ctx: &mut Ctx) {
    ctx.table_cells.clear();
    walk_children(el, ctx);
    let row = std::mem::take(&mut ctx.table_cells);
    if !row.is_empty() {
        ctx.table_rows.push(row);
    }
}

fn handle_td(el: ElementRef, ctx: &mut Ctx) {
    let was_in_cell = ctx.in_cell;
    let saved_buf = std::mem::take(&mut ctx.cell_buf);
    ctx.in_cell = true;
    walk_children(el, ctx);
    let cell = ctx.cell_buf.trim().to_string();
    ctx.cell_buf = saved_buf;
    ctx.in_cell = was_in_cell;
    ctx.table_cells.push(cell);
}

// ============================== Helpers ==============================

fn inner_markdown(el: ElementRef, ctx: &mut Ctx) -> String {
    let saved = std::mem::take(&mut ctx.out);
    walk_children(el, ctx);
    std::mem::replace(&mut ctx.out, saved)
}

fn collapse_whitespace(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut last_was_ws = false;
    for ch in text.chars() {
        if ch.is_whitespace() {
            if !last_was_ws {
                result.push(' ');
                last_was_ws = true;
            }
        } else {
            result.push(ch);
            last_was_ws = false;
        }
    }
    result
}

fn escape_markdown(text: &str) -> String {
    let mut result = String::with_capacity(text.len() + 8);
    for ch in text.chars() {
        match ch {
            '\\' => result.push_str("\\\\" ),
            '[' | ']' | '*' | '_' | '~' => {
                result.push('\\');
                result.push(ch);
            }
            _ => result.push(ch),
        }
    }
    result
}

fn ensure_blank_line(out: &mut String) {
    let trimmed_len = out.trim_end().len();
    if trimmed_len == 0 {
        out.clear();
        return;
    }
    out.truncate(trimmed_len);
    out.push_str("\n\n");
}

fn collapse_blank_lines(s: String) -> String {
    let re = Regex::new(r"\n{3,}").unwrap();
    re.replace_all(&s, "\n\n").into_owned()
}

fn strip_p_in_cell(inner: &str) -> String {
    let re_p_start = Regex::new(r"(?i)^\s*<p>").unwrap();
    let re_p_end = Regex::new(r"(?i)</p>\s*$").unwrap();
    let re_p_mid = Regex::new(r"(?i)</p>\s*<p>").unwrap();
    let s = re_p_start.replace(inner, "");
    let s = re_p_end.replace(&s, "");
    let s = re_p_mid.replace_all(&s, " ");
    s.into_owned()
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_headings() {
        assert_eq!(html_to_markdown("<h1>Hello World</h1>"), "# Hello World");
        assert_eq!(html_to_markdown("<h2>Section 1.</h2>"), "## Section 1.");
        assert_eq!(html_to_markdown("<h3>Third</h3>"), "### Third");
    }

    #[test]
    fn test_heading_with_emphasis() {
        assert_eq!(
            html_to_markdown("<h1>Hello <em>World</em></h1>"),
            "# Hello _World_"
        );
    }

    #[test]
    fn test_paragraphs() {
        assert_eq!(
            html_to_markdown("<p>Hello</p><p>World</p>"),
            "Hello\n\nWorld"
        );
    }

    #[test]
    fn test_bold_italic_strike() {
        assert_eq!(html_to_markdown("<strong>bold</strong>"), "**bold**");
        assert_eq!(html_to_markdown("<em>italic</em>"), "_italic_");
        assert_eq!(html_to_markdown("<del>deleted</del>"), "~~deleted~~");
        assert_eq!(html_to_markdown("<s>struck</s>"), "~~struck~~");
    }

    #[test]
    fn test_links() {
        assert_eq!(
            html_to_markdown(r#"<a href="https://example.com">link</a>"#),
            "[link](https://example.com)"
        );
    }

    #[test]
    fn test_images() {
        assert_eq!(
            html_to_markdown(r#"<img src="photo.jpg" alt="a photo">"#),
            "![a photo](photo.jpg)"
        );
    }

    #[test]
    fn test_inline_code() {
        assert_eq!(html_to_markdown("<code>hello</code>"), "`hello`");
    }

    #[test]
    fn test_code_block_with_lang() {
        let html = r#"<pre><code class="language-js">const x = 1;
console.log(x);</code></pre>"#;
        assert_eq!(
            html_to_markdown(html),
            "```js\nconst x = 1;\nconsole.log(x);\n```"
        );
    }

    #[test]
    fn test_code_block_no_lang() {
        assert_eq!(
            html_to_markdown("<pre><code>plain code\nline 2</code></pre>"),
            "```\nplain code\nline 2\n```"
        );
    }

    #[test]
    fn test_bullet_list() {
        assert_eq!(
            html_to_markdown("<ul><li>A</li><li>B</li></ul>"),
            "- A\n- B"
        );
    }

    #[test]
    fn test_nested_list() {
        let html = "<ul><li>Item 1</li><li>Item 2<ul><li>Nested A</li><li>Nested B</li></ul></li><li>Item 3</li></ul>";
        assert_eq!(
            html_to_markdown(html),
            "- Item 1\n- Item 2\n  - Nested A\n  - Nested B\n- Item 3"
        );
    }

    #[test]
    fn test_ordered_list_with_start() {
        assert_eq!(
            html_to_markdown(r#"<ol start="3"><li>Third</li><li>Fourth</li></ol>"#),
            "3. Third\n4. Fourth"
        );
    }

    #[test]
    fn test_table_no_thead() {
        let html = normalize_tables_html(
            "<table><tr><td>Name</td><td>Age</td></tr><tr><td>Alice</td><td>30</td></tr></table>",
        );
        assert_eq!(
            html_to_markdown(&html),
            "| Name | Age |\n| --- | --- |\n| Alice | 30 |"
        );
    }

    #[test]
    fn test_table_with_thead() {
        let html = "<table><thead><tr><th>Name</th><th>Age</th></tr></thead><tbody><tr><td>Alice</td><td>30</td></tr></tbody></table>";
        assert_eq!(
            html_to_markdown(html),
            "| Name | Age |\n| --- | --- |\n| Alice | 30 |"
        );
    }

    #[test]
    fn test_blockquote() {
        assert_eq!(
            html_to_markdown("<blockquote><p>A quote</p></blockquote>"),
            "> A quote"
        );
    }

    #[test]
    fn test_nested_blockquote() {
        let html =
            "<blockquote><p>First level</p><blockquote><p>Nested quote</p></blockquote></blockquote>";
        assert_eq!(
            html_to_markdown(html),
            "> First level\n>\n> > Nested quote"
        );
    }

    #[test]
    fn test_hr() {
        assert_eq!(
            html_to_markdown("<p>Above</p><hr><p>Below</p>"),
            "Above\n\n* * *\n\nBelow"
        );
    }

    #[test]
    fn test_br() {
        assert_eq!(
            html_to_markdown("<p>Line 1<br>Line 2<br>Line 3</p>"),
            "Line 1  \nLine 2  \nLine 3"
        );
    }

    #[test]
    fn test_escape_markdown_chars() {
        assert_eq!(
            html_to_markdown("<p>Use *asterisks* and _underscores_ literally.</p>"),
            "Use \\*asterisks\\* and \\_underscores\\_ literally."
        );
    }

    #[test]
    fn test_escape_brackets() {
        assert_eq!(html_to_markdown("<p>[special]</p>"), "\\[special\\]");
    }

    #[test]
    fn test_whitespace_collapsing() {
        assert_eq!(
            html_to_markdown("<p>  Multiple   spaces   here  </p>"),
            "Multiple spaces here"
        );
    }

    #[test]
    fn test_normalize_tables_strip_p() {
        let input = "<table><tr><td><p>Name</p></td><td><p>Age</p></td></tr></table>";
        let result = normalize_tables_html(input);
        assert!(result.contains("<th>Name</th>"), "got: {result}");
        assert!(!result.contains("<p>"), "got: {result}");
    }
}