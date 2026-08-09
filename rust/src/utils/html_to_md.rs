//! HTML → Markdown engine. Rust equivalent of src/utils/turndown.ts.
//!
//! Architecture follows turndown's process/join composition model:
//! - process(node) reduces over children, each producing a replacement string
//! - join(output, replacement) trims trailing/leading newlines, inserts separator
//! - replacementForNode applies rule.replacement(content, node) with flanking whitespace

use regex::Regex;
use scraper::{ElementRef, Html, Node};

// ============================== Public API ==============================

/// Convert an HTML fragment or document to GFM markdown.
pub fn html_to_markdown(html: &str) -> String {
    let has_html_tag =
        html.contains("<html") || html.contains("<!DOCTYPE") || html.contains("<!doctype");
    let doc = if has_html_tag {
        Html::parse_document(html)
    } else {
        Html::parse_fragment(html)
    };
    let root = doc.root_element();
    let mut ctx = Ctx::default();
    let output = process(root, &mut ctx);
    post_process(&output)
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
    list_stack: Vec<ListCtx>,
    in_pre: bool,
    in_code: bool,
    pre_no_code: bool,
    in_table: bool,
    table_rows: Vec<Vec<String>>,
    table_cells: Vec<String>,
    in_cell: bool,
    in_heading: bool,
}

struct ListCtx {
    ordered: bool,
    start: usize,
    item_index: usize,
}

// ============================== Core: process / join / replacementForNode ==============================

/// Reduces a DOM node to its Markdown string equivalent by reducing over children.
fn process(parent: ElementRef, ctx: &mut Ctx) -> String {
    let mut output = String::new();
    let mut prev_text_ends_with_space = true;

    for child in parent.children() {
        match child.value() {
            Node::Text(text) => {
                if ctx.in_table && !ctx.in_cell {
                    continue;
                }
                let replacement =
                    text_replacement(&text.text, ctx, &output, prev_text_ends_with_space);
                if !replacement.is_empty() {
                    output = join_str(&output, &replacement);
                    prev_text_ends_with_space = replacement.ends_with(' ');
                }
            }
            Node::Element(_) => {
                if let Some(child_el) = ElementRef::wrap(child) {
                    let tag = child_el.value().name().to_lowercase();

                    // Strip trailing space before block elements and <br>
                    if is_block_or_br(&tag) {
                        let trimmed = output.trim_end_matches(' ');
                        output.truncate(trimmed.len());
                        prev_text_ends_with_space = true;
                    }

                    if tag == "script" || tag == "style" {
                        continue;
                    }

                    let replacement = replacement_for_node(child_el, ctx);
                    if !replacement.is_empty() {
                        output = join_str(&output, &replacement);
                        if is_block_or_br(&tag) {
                            prev_text_ends_with_space = true;
                        } else {
                            prev_text_ends_with_space = output.ends_with(' ');
                        }
                    }
                }
            }
            _ => {}
        }
    }
    // Strip trailing space (mirrors turndown's collapseWhitespace final cleanup)
    // Don't strip in preformatted blocks - spaces are significant
    if !ctx.in_pre {
        let trimmed = output.trim_end_matches(' ');
        output.truncate(trimmed.len());
    }
    output
}

/// Produce the replacement string for a text node.
fn text_replacement(
    text: &str,
    ctx: &Ctx,
    current_output: &str,
    prev_ends_space: bool,
) -> String {
    if ctx.in_pre {
        if ctx.pre_no_code {
            return escape_markdown(text);
        }
        return text.to_string();
    }
    if ctx.in_code {
        return text.to_string();
    }
    let collapsed = collapse_whitespace(text);
    if collapsed.is_empty() {
        return String::new();
    }
    let collapsed = if collapsed.starts_with(' ')
        && (current_output.is_empty() || current_output.ends_with('\n') || prev_ends_space)
    {
        &collapsed[1..]
    } else {
        &collapsed
    };
    if collapsed.is_empty() {
        return String::new();
    }
    escape_markdown(collapsed)
}

/// Convert an element node to its Markdown replacement string.
fn replacement_for_node(el: ElementRef, ctx: &mut Ctx) -> String {
    let tag = el.value().name().to_lowercase();

    // Check if blank (turndown's isBlank)
    if !ctx.in_pre && is_blank_node(el) {
        return if is_block_tag(&tag) || is_block_or_br(&tag) {
            "\n\n".to_string()
        } else {
            String::new()
        };
    }

    match tag.as_str() {
        "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => rule_heading(el, ctx, &tag),
        "p" => rule_paragraph(el, ctx),
        "br" => rule_br(),
        "hr" => rule_hr(),
        "strong" | "b" => rule_wrap(el, ctx, "**"),
        "em" | "i" => rule_wrap(el, ctx, "_"),
        "del" | "s" | "strike" => rule_wrap(el, ctx, "~~"),
        "a" => rule_link(el, ctx),
        "img" => rule_img(el),
        "code" => rule_code(el, ctx),
        "pre" => rule_pre(el, ctx),
        "blockquote" => rule_blockquote(el, ctx),
        "ul" => rule_list(el, ctx, false),
        "ol" => rule_list(el, ctx, true),
        "li" => rule_li(el, ctx),
        "table" => rule_table(el, ctx),
        "thead" | "tbody" | "tfoot" => process(el, ctx),
        "tr" => rule_tr(el, ctx),
        "td" | "th" => rule_td(el, ctx),
        _ => {
            let content = process(el, ctx);
            if is_block_tag(&tag) {
                format!("\n\n{}\n\n", content)
            } else {
                content
            }
        }
    }
}

/// Turndown's join()
fn join_str(left: &str, right: &str) -> String {
    let s1 = trim_trailing_newlines(left);
    let s2 = trim_leading_newlines(right);
    let left_trailing = left.len() - s1.len();
    let right_leading = right.len() - s2.len();
    let nls = std::cmp::max(left_trailing, right_leading);
    let nls = std::cmp::min(nls, 2);
    let separator = &"\n\n"[..nls];
    format!("{}{}{}", s1, separator, s2)
}

fn trim_trailing_newlines(s: &str) -> &str {
    let bytes = s.as_bytes();
    let mut end = bytes.len();
    while end > 0 && bytes[end - 1] == b'\n' {
        end -= 1;
    }
    &s[..end]
}

fn trim_leading_newlines(s: &str) -> &str {
    let bytes = s.as_bytes();
    let mut start = 0;
    while start < bytes.len() && bytes[start] == b'\n' {
        start += 1;
    }
    &s[start..]
}

fn post_process(output: &str) -> String {
    let s = output
        .trim_start_matches(|c: char| c == '\t' || c == '\r' || c == '\n')
        .trim_end_matches(|c: char| c == '\t' || c == '\r' || c == '\n' || c == ' ');
    s.to_string()
}

// ============================== isBlank ==============================

fn is_blank_node(el: ElementRef) -> bool {
    let tag = el.value().name().to_lowercase();
    if is_void_tag(&tag) {
        return false;
    }
    if is_meaningful_when_blank(&tag) {
        return false;
    }
    let text: String = el.text().collect();
    if !text.trim().is_empty() {
        return false;
    }
    if has_void_descendant(el) || has_meaningful_descendant(el) {
        return false;
    }
    true
}

fn is_void_tag(tag: &str) -> bool {
    matches!(
        tag,
        "area" | "base" | "br" | "col" | "command" | "embed" | "hr" | "img"
            | "input" | "keygen" | "link" | "meta" | "param" | "source" | "track" | "wbr"
    )
}

fn is_meaningful_when_blank(tag: &str) -> bool {
    matches!(
        tag,
        "a" | "table" | "thead" | "tbody" | "tfoot" | "th" | "td"
            | "iframe" | "script" | "audio" | "video"
    )
}

fn has_void_descendant(el: ElementRef) -> bool {
    for desc in el.descendants() {
        if let Node::Element(e) = desc.value() {
            if is_void_tag(&e.name().to_lowercase()) {
                return true;
            }
        }
    }
    false
}

fn has_meaningful_descendant(el: ElementRef) -> bool {
    for desc in el.descendants() {
        if let Node::Element(e) = desc.value() {
            if is_meaningful_when_blank(&e.name().to_lowercase()) {
                return true;
            }
        }
    }
    false
}

// ============================== Flanking whitespace ==============================

fn flanking_whitespace(el: ElementRef) -> (String, String) {
    let tag = el.value().name().to_lowercase();
    if is_block_tag(&tag) || is_block_or_br(&tag) {
        return (String::new(), String::new());
    }

    // If element contains block descendants, skip flanking ws 
    // (turndown's collapseWhitespace would have stripped inter-block whitespace)
    if has_block_descendant(el) {
        return (String::new(), String::new());
    }
    // Use collapsed text content to detect edges
    let raw_text: String = el.text().collect();
    let text_content = collapse_whitespace(&raw_text);
    // Only detect flanking ws if text content actually starts/ends with whitespace
    if !text_content.starts_with(' ') && !text_content.ends_with(' ') {
        return (String::new(), String::new());
    }
    let edges = edge_whitespace(&text_content);

    let mut leading = edges.leading;
    let mut trailing = edges.trailing;

    if !edges.leading_ascii.is_empty() && is_flanked_by_whitespace_left(el) {
        leading = edges.leading_non_ascii.to_string();
    }

    if !edges.trailing_ascii.is_empty() && is_flanked_by_whitespace_right(el) {
        trailing = edges.trailing_non_ascii.to_string();
    }

    (leading, trailing)
}

struct EdgeWhitespace {
    leading: String,
    leading_ascii: String,
    leading_non_ascii: String,
    trailing: String,
    trailing_ascii: String,
    trailing_non_ascii: String,
}

fn edge_whitespace(s: &str) -> EdgeWhitespace {
    let chars: Vec<char> = s.chars().collect();
    let len = chars.len();

    let mut i = 0;
    while i < len && is_ascii_ws(chars[i]) {
        i += 1;
    }
    let leading_ascii_end = i;

    while i < len && chars[i].is_whitespace() && !is_ascii_ws(chars[i]) {
        i += 1;
    }
    let leading_end = i;

    let mut last_non_ws = None;
    for j in (0..len).rev() {
        if !chars[j].is_whitespace() {
            last_non_ws = Some(j);
            break;
        }
    }

    let (trailing, trailing_non_ascii, trailing_ascii) = if let Some(last) = last_non_ws {
        let trail_start = last + 1;
        let mut k = len;
        while k > trail_start && is_ascii_ws(chars[k - 1]) {
            k -= 1;
        }
        let trailing_ascii_start = k;
        let trailing_str: String = chars[trail_start..].iter().collect();
        let trailing_non_ascii_str: String =
            chars[trail_start..trailing_ascii_start].iter().collect();
        let trailing_ascii_str: String = chars[trailing_ascii_start..].iter().collect();
        (trailing_str, trailing_non_ascii_str, trailing_ascii_str)
    } else {
        (String::new(), String::new(), String::new())
    };

    let leading: String = chars[..leading_end].iter().collect();
    let leading_ascii: String = chars[..leading_ascii_end].iter().collect();
    let leading_non_ascii: String = chars[leading_ascii_end..leading_end].iter().collect();

    if last_non_ws.is_none() {
        return EdgeWhitespace {
            leading: s.to_string(),
            leading_ascii,
            leading_non_ascii,
            trailing: String::new(),
            trailing_ascii: String::new(),
            trailing_non_ascii: String::new(),
        };
    }

    EdgeWhitespace {
        leading,
        leading_ascii,
        leading_non_ascii,
        trailing,
        trailing_ascii,
        trailing_non_ascii,
    }
}

fn has_block_descendant(el: ElementRef) -> bool {
    for desc in el.descendants() {
        if let Node::Element(e) = desc.value() {
            if is_block_tag(&e.name().to_lowercase()) || is_block_or_br(&e.name().to_lowercase()) {
                return true;
            }
        }
    }
    false
}

fn is_ascii_ws(c: char) -> bool {
    c == ' ' || c == '\t' || c == '\r' || c == '\n'
}

fn is_flanked_by_whitespace_left(el: ElementRef) -> bool {
    if let Some(sibling) = el.prev_sibling() {
        match sibling.value() {
            Node::Text(t) => t.text.ends_with(' '),
            Node::Element(_) => {
                if let Some(sib_el) = ElementRef::wrap(sibling) {
                    if !is_block_tag(&sib_el.value().name().to_lowercase()) {
                        let text: String = sib_el.text().collect();
                        text.ends_with(' ')
                    } else {
                        false
                    }
                } else {
                    false
                }
            }
            _ => false,
        }
    } else {
        false
    }
}

fn is_flanked_by_whitespace_right(el: ElementRef) -> bool {
    if let Some(sibling) = el.next_sibling() {
        match sibling.value() {
            Node::Text(t) => {
                let s = &t.text;
                s.starts_with(' ') || s.starts_with('\n') || s.starts_with('\t')
            }
            Node::Element(_) => {
                if let Some(sib_el) = ElementRef::wrap(sibling) {
                    if !is_block_tag(&sib_el.value().name().to_lowercase()) {
                        let text: String = sib_el.text().collect();
                        text.starts_with(' ')
                    } else {
                        false
                    }
                } else {
                    false
                }
            }
            _ => false,
        }
    } else {
        false
    }
}

// ============================== Rule implementations ==============================

fn rule_heading(el: ElementRef, ctx: &mut Ctx, tag: &str) -> String {
    let level: usize = tag[1..].parse().unwrap_or(1);
    let prefix = "#".repeat(level);
    let was = ctx.in_heading;
    ctx.in_heading = true;
    let content = process(el, ctx);
    ctx.in_heading = was;
    let cleaned = content.replace(r"\.", ".").trim().to_string();
    format!("\n\n{prefix} {cleaned}\n\n")
}

fn rule_paragraph(el: ElementRef, ctx: &mut Ctx) -> String {
    if ctx.in_cell {
        return process(el, ctx);
    }
    let content = process(el, ctx);
    format!("\n\n{}\n\n", content)
}

fn rule_br() -> String {
    "  \n".to_string()
}

fn rule_hr() -> String {
    "\n\n* * *\n\n".to_string()
}

fn rule_wrap(el: ElementRef, ctx: &mut Ctx, marker: &str) -> String {
    let content = process(el, ctx);
    if content.trim().is_empty() {
        return String::new();
    }

    // Only apply flanking whitespace if content actually has leading/trailing space
    let has_leading_space = content.starts_with(' ');
    let has_trailing_space = content.ends_with(' ');
    let (leading, trailing) = if has_leading_space || has_trailing_space {
        flanking_whitespace(el)
    } else {
        (String::new(), String::new())
    };
    let inner = if !leading.is_empty() || !trailing.is_empty() {
        content.trim().to_string()
    } else {
        content
    };

    format!("{leading}{marker}{inner}{marker}{trailing}")
}

fn rule_link(el: ElementRef, ctx: &mut Ctx) -> String {
    let raw_href = el.value().attr("href").unwrap_or("");
    if raw_href.is_empty() {
        return process(el, ctx);
    }

    let href = escape_url_parens(raw_href);
    let title = el.value().attr("title");
    let content = process(el, ctx);

    // Only apply flanking whitespace if content actually has leading/trailing space
    let has_leading_space = content.starts_with(' ');
    let has_trailing_space = content.ends_with(' ');
    let (leading, trailing) = if has_leading_space || has_trailing_space {
        flanking_whitespace(el)
    } else {
        (String::new(), String::new())
    };
    let inner = if !leading.is_empty() || !trailing.is_empty() {
        content.trim().to_string()
    } else {
        content
    };

    let title_part = match title {
        Some(t) if !t.is_empty() => {
            let escaped = t.replace('"', r#"\""#);
            format!(r#" "{escaped}""#)
        }
        _ => String::new(),
    };
    format!("{leading}[{inner}]({href}{title_part}){trailing}")
}

fn rule_img(el: ElementRef) -> String {
    let alt = el.value().attr("alt").unwrap_or("");
    let raw_src = el.value().attr("src").unwrap_or("");
    if raw_src.is_empty() {
        return String::new();
    }
    let src = escape_url_parens(raw_src);
    let title = el.value().attr("title");
    let title_part = match title {
        Some(t) if !t.is_empty() => {
            let escaped = t.replace('"', r#"\""#);
            format!(r#" "{escaped}""#)
        }
        _ => String::new(),
    };
    format!("![{alt}]({src}{title_part})")
}

fn rule_code(el: ElementRef, ctx: &mut Ctx) -> String {
    if ctx.in_pre {
        let was_code = ctx.in_code;
        ctx.in_code = true;
        let result = process(el, ctx);
        ctx.in_code = was_code;
        return result;
    }
    let text = el.text().collect::<String>();
    if text.is_empty() {
        return String::new();
    }
    let content = text.replace(|c: char| c == '\r' || c == '\n', " ");
    let extra_space =
        if content.starts_with('`') || content.ends_with('`')
            || (content.starts_with(' ')
                && content.ends_with(' ')
                && content.chars().any(|c| c != ' '))
        {
            " "
        } else {
            ""
        };
    let mut delimiter = "`".to_string();
    let backtick_re = Regex::new(r"`+").unwrap();
    let matches: Vec<String> = backtick_re
        .find_iter(&content)
        .map(|m| m.as_str().to_string())
        .collect();
    while matches.contains(&delimiter) {
        delimiter.push('`');
    }
    format!("{delimiter}{extra_space}{content}{extra_space}{delimiter}")
}

fn rule_pre(el: ElementRef, ctx: &mut Ctx) -> String {
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

    if let Some(_code_el) = code_child {
        let mut fence_size: usize = 3;
        ctx.in_pre = true;
        ctx.in_code = true;
        let code_text = process(el, ctx);
        ctx.in_pre = false;
        ctx.in_code = false;

        let fence_re = Regex::new(r"(?m)^`{3,}").unwrap();
        for m in fence_re.find_iter(&code_text) {
            if m.as_str().len() >= fence_size {
                fence_size = m.as_str().len() + 1;
            }
        }

        let fence: String = std::iter::repeat('`').take(fence_size).collect();
        let lang_str = lang.as_deref().unwrap_or("");
        let code_trimmed = code_text.trim_end_matches('\n');
        format!("\n\n{fence}{lang_str}\n{code_trimmed}\n{fence}\n\n")
    } else {
        ctx.in_pre = true;
        ctx.pre_no_code = true;
        let content = process(el, ctx);
        ctx.in_pre = false;
        ctx.pre_no_code = false;
        format!("\n\n{}\n\n", content)
    }
}

fn rule_blockquote(el: ElementRef, ctx: &mut Ctx) -> String {
    let content = process(el, ctx);
    let trimmed = content
        .trim_start_matches('\n')
        .trim_end_matches('\n');
    let quoted = trimmed
        .split('\n')
        .map(|line| {
            if line.is_empty() {
                ">".to_string()
            } else {
                format!("> {}", line)
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    format!("\n\n{}\n\n", quoted)
}

fn rule_list(el: ElementRef, ctx: &mut Ctx, ordered: bool) -> String {
    let start: usize = if ordered {
        el.value()
            .attr("start")
            .and_then(|s| s.parse().ok())
            .unwrap_or(1)
    } else {
        1
    };

    ctx.list_stack.push(ListCtx {
        ordered,
        start,
        item_index: 0,
    });

    let mut output = String::new();
    for child in el.children() {
        if let Some(child_el) = ElementRef::wrap(child) {
            let replacement = replacement_for_node(child_el, ctx);
            if !replacement.is_empty() {
                output = join_str(&output, &replacement);
            }
        }
    }

    ctx.list_stack.pop();

    let parent_is_li = el
        .parent()
        .and_then(|p| p.value().as_element().map(|e| e.name().eq_ignore_ascii_case("li")))
        .unwrap_or(false);

    let is_last_child = if parent_is_li {
        el.parent()
            .map(|p| {
                p.children()
                    .filter_map(|c| ElementRef::wrap(c))
                    .last()
                    .map(|last| last.id() == el.id())
                    .unwrap_or(false)
            })
            .unwrap_or(false)
    } else {
        false
    };

    if parent_is_li && is_last_child {
        format!("\n{}", output)
    } else {
        format!("\n\n{}\n\n", output)
    }
}

fn rule_li(el: ElementRef, ctx: &mut Ctx) -> String {
    let depth = ctx.list_stack.len();
    if depth == 0 {
        return process(el, ctx);
    }

    let list_ctx = ctx.list_stack.last_mut().unwrap();
    let prefix = if list_ctx.ordered {
        let num = list_ctx.start + list_ctx.item_index;
        format!("{num}. ")
    } else {
        "- ".to_string()
    };
    list_ctx.item_index += 1;

    let content = process(el, ctx);

    let is_paragraph = content.ends_with('\n');
    let trimmed = content
        .trim_start_matches('\n')
        .trim_end_matches('\n');
    let with_trailing = if is_paragraph {
        format!("{}\n", trimmed)
    } else {
        trimmed.to_string()
    };
    let indented = with_trailing.replace('\n', "\n  ");

    let has_next = el.next_sibling().is_some();
    format!(
        "{}{}{}",
        prefix,
        indented,
        if has_next { "\n" } else { "" }
    )
}

// ============================== Table handling ==============================

fn rule_table(el: ElementRef, ctx: &mut Ctx) -> String {
    let was_in_table = ctx.in_table;
    let saved_rows = std::mem::take(&mut ctx.table_rows);
    let saved_cells = std::mem::take(&mut ctx.table_cells);
    ctx.in_table = true;

    process(el, ctx);

    ctx.in_table = was_in_table;

    if ctx.table_rows.is_empty() {
        ctx.table_rows = saved_rows;
        ctx.table_cells = saved_cells;
        return String::new();
    }

    let mut result = String::new();
    let header = &ctx.table_rows[0];
    let col_count = header.len();

    result.push_str("| ");
    result.push_str(&header.join(" | "));
    result.push_str(" |\n");

    result.push_str("| ");
    result.push_str(&vec!["---"; col_count].join(" | "));
    result.push_str(" |\n");

    for row in &ctx.table_rows[1..] {
        result.push_str("| ");
        let mut padded = row.clone();
        while padded.len() < col_count {
            padded.push(String::new());
        }
        result.push_str(&padded.join(" | "));
        result.push_str(" |\n");
    }

    ctx.table_rows = saved_rows;
    ctx.table_cells = saved_cells;

    format!("\n\n{}\n", result)
}

fn rule_tr(el: ElementRef, ctx: &mut Ctx) -> String {
    ctx.table_cells.clear();
    for child in el.children() {
        if let Some(child_el) = ElementRef::wrap(child) {
            replacement_for_node(child_el, ctx);
        }
    }
    let row = std::mem::take(&mut ctx.table_cells);
    if !row.is_empty() {
        ctx.table_rows.push(row);
    }
    String::new()
}

fn rule_td(el: ElementRef, ctx: &mut Ctx) -> String {
    let was_in_cell = ctx.in_cell;
    ctx.in_cell = true;
    let content = process(el, ctx);
    ctx.in_cell = was_in_cell;
    ctx.table_cells.push(content.trim().to_string());
    String::new()
}

// ============================== Helpers ==============================

fn collapse_whitespace(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut last_was_ws = false;
    for ch in text.chars() {
        if ch.is_ascii_whitespace() {
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
    let s = text.replace('\\', "\\\\");
    let mut result = String::with_capacity(s.len() + 8);
    for ch in s.chars() {
        match ch {
            '[' | ']' | '*' | '_' | '~' | '`' | '|' => {
                result.push('\\');
                result.push(ch);
            }
            _ => result.push(ch),
        }
    }

    if result.starts_with('-') {
        result = format!("\\-{}", &result[1..]);
    } else if result.starts_with("+ ") {
        result = format!("\\+ {}", &result[2..]);
    } else if result.starts_with('>') {
        result = format!("\\>{}", &result[1..]);
    } else if result.starts_with("\\~\\~\\~") {
        result = format!("\\{}", &result);
    } else {
        let eq_count = result.chars().take_while(|&c| c == '=').count();
        if eq_count > 0 {
            result = format!("\\{}", &result);
        } else {
            let hash_count = result.chars().take_while(|&c| c == '#').count();
            if (1..=6).contains(&hash_count) && result.as_bytes().get(hash_count) == Some(&b' ')
            {
                result = format!("\\{}", &result);
            } else {
                let digit_count = result
                    .chars()
                    .take_while(|c| c.is_ascii_digit())
                    .count();
                if digit_count > 0 && result[digit_count..].starts_with(". ") {
                    let digits = &result[..digit_count];
                    result = format!("{}\\. {}", digits, &result[digit_count + 2..]);
                }
            }
        }
    }

    result
}

fn escape_url_parens(url: &str) -> String {
    url.replace('(', r"\(").replace(')', r"\)")
}

fn is_block_or_br(tag: &str) -> bool {
    tag == "br"
        || matches!(
            tag,
            "h1" | "h2"
                | "h3"
                | "h4"
                | "h5"
                | "h6"
                | "p"
                | "pre"
                | "blockquote"
                | "table"
                | "thead"
                | "tbody"
                | "tfoot"
                | "tr"
                | "td"
                | "th"
                | "ul"
                | "ol"
                | "li"
                | "hr"
        )
        || is_block_tag(tag)
}

fn is_block_tag(tag: &str) -> bool {
    matches!(
        tag,
        "address"
            | "article"
            | "aside"
            | "center"
            | "dd"
            | "details"
            | "dialog"
            | "dir"
            | "div"
            | "dl"
            | "dt"
            | "fieldset"
            | "figcaption"
            | "figure"
            | "footer"
            | "form"
            | "header"
            | "hgroup"
            | "main"
            | "nav"
            | "ol"
            | "output"
            | "section"
            | "summary"
            | "ul"
    )
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

    #[test]
    fn test_link_with_title() {
        assert_eq!(
            html_to_markdown(r#"<a href="https://example.com" title="Example Site">link</a>"#),
            r#"[link](https://example.com "Example Site")"#
        );
    }

    #[test]
    fn test_link_title_with_quotes() {
        assert_eq!(
            html_to_markdown(r#"<a href="/page" title='Say "hi"'>text</a>"#),
            "[text](/page \"Say \\\"hi\\\"\")"
        );
    }

    #[test]
    fn test_img_with_title() {
        assert_eq!(
            html_to_markdown(r#"<img src="photo.jpg" alt="pic" title="My Photo">"#),
            r#"![pic](photo.jpg "My Photo")"#
        );
    }

    #[test]
    fn test_url_parens_escaped() {
        assert_eq!(
            html_to_markdown(r#"<a href="https://en.wikipedia.org/wiki/Rust_(language)">Rust</a>"#),
            r##"[Rust](https://en.wikipedia.org/wiki/Rust_\(language\))"##
        );
    }

    #[test]
    fn test_block_elements_produce_blank_lines() {
        assert_eq!(
            html_to_markdown("<div>Block 1</div><div>Block 2</div>"),
            "Block 1

Block 2"
        );
    }

    #[test]
    fn test_nav_section_are_blocks() {
        assert_eq!(
            html_to_markdown("<nav>Navigation</nav><section>Content</section>"),
            "Navigation

Content"
        );
    }

    #[test]
    fn test_link_in_table_cell() {
        let html = normalize_tables_html(
            r#"<table><tr><td><a href="https://example.com" title="Ex">Paradigms</a></td><td>val</td></tr></table>"#
        );
        let result = html_to_markdown(&html);
        assert!(result.contains(r#"[Paradigms](https://example.com "Ex")"#), "got: {result}");
    }

    #[test]
    fn test_escape_pipe() {
        assert_eq!(
            html_to_markdown("<p>a | b</p>"),
            "a \\| b"
        );
    }

    #[test]
    fn test_footnote_brackets_escaped() {
        let html = r##"<a href="#cite"><span class="cite-bracket">[</span>1<span class="cite-bracket">]</span></a>"##;
        assert_eq!(
            html_to_markdown(html),
            "[\\[1\\]](#cite)"
        );
    }

    #[test]
    fn test_div_inside_link() {
        let html = r##"<a href="#"><div>Top</div></a>"##;
        let result = html_to_markdown(html);
        assert!(!result.contains("\t"), "Output should not contain tabs: {:?}", result);
        assert!(result.contains("["), "Should contain link: {:?}", result);
    }

    #[test]
    fn test_pre_without_code_escapes() {
        let html = r#"<pre><span>let x = 10;</span></pre>"#;
        let result = html_to_markdown(html);
        assert!(result.contains(r"="), "Expected escaped = in pre without code, got: {result}");
    }

}