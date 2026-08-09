//! HTML → Markdown engine. Rust equivalent of src/utils/turndown.ts.
//!
//! Architecture follows turndown's process/join composition model:
//! - process(node) reduces over children, each producing a replacement string
//! - join(output, replacement) trims trailing/leading newlines, inserts separator
//! - replacementForNode applies rule.replacement(content, node) with flanking whitespace

use std::collections::HashMap;

use ego_tree::NodeId;
use regex::Regex;
use scraper::{ElementRef, Html, Node};

// ============================== Public API ==============================

/// Convert an HTML fragment or document to GFM markdown.
pub fn html_to_markdown(html: &str) -> String {
    // html5ever parses <noscript> children as raw text (scripting flag on);
    // domino (turndown's DOM) parses them as elements. Rewrite to <div>, which
    // turndown treats identically (block defaultReplacement), so the content
    // converts the same way.
    let noscript_open = Regex::new(r"(?i)<noscript(\s[^>]*)?>").unwrap();
    let noscript_close = Regex::new(r"(?i)</noscript>").unwrap();
    let html = noscript_open.replace_all(html, "<div$1>");
    let html = noscript_close.replace_all(&html, "</div>");
    let html = html.as_ref();

    let has_html_tag =
        html.contains("<html") || html.contains("<!DOCTYPE") || html.contains("<!doctype");
    let doc = if has_html_tag {
        Html::parse_document(html)
    } else {
        Html::parse_fragment(html)
    };
    let root = doc.root_element();
    let (collapsed, removed) = collapse_whitespace_pass(root);
    let mut ctx = Ctx {
        collapsed,
        removed,
        ..Default::default()
    };
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


// ============================== collapseWhitespace pre-pass ==============================
// Exact port of turndown's collapseWhitespace (collapse-whitespace package):
// a single document-order traversal carrying prevText/keepLeadingWs state.
// We cannot mutate scraper's DOM, so the result is a NodeId → collapsed-text
// map; nodes that turndown would remove are simply absent from the map.

/// turndown's blockElements list (node name match, case-insensitive).
fn td_is_block(tag: &str) -> bool {
    matches!(
        tag,
        "address" | "article" | "aside" | "audio" | "blockquote" | "body" | "canvas"
            | "center" | "dd" | "dir" | "div" | "dl" | "dt" | "fieldset" | "figcaption"
            | "figure" | "footer" | "form" | "frameset" | "h1" | "h2" | "h3" | "h4" | "h5"
            | "h6" | "header" | "hgroup" | "hr" | "html" | "isindex" | "li" | "main" | "menu"
            | "nav" | "noframes" | "noscript" | "ol" | "output" | "p" | "pre" | "section"
            | "table" | "tbody" | "td" | "tfoot" | "th" | "thead" | "tr" | "ul"
    )
}

/// turndown's voidElements list.
fn td_is_void(tag: &str) -> bool {
    matches!(
        tag,
        "area" | "base" | "br" | "col" | "command" | "embed" | "hr" | "img" | "input"
            | "keygen" | "link" | "meta" | "param" | "source" | "track" | "wbr"
    )
}

fn node_tag(node: &ego_tree::NodeRef<Node>) -> Option<String> {
    node.value().as_element().map(|e| e.name().to_lowercase())
}

fn node_is_pre(node: &ego_tree::NodeRef<Node>) -> bool {
    node_tag(node).as_deref() == Some("pre")
}

/// turndown's next(prev, current, isPre) traversal step.
/// Turndown physically removes emptied text/comment nodes from the DOM;
/// we can't mutate scraper's tree, so traversal skips nodes in `removed`
/// (all removable nodes are leaves — text/comments — so child/sibling
/// skipping is sufficient).
fn skip_first_child<'a>(
    node: &ego_tree::NodeRef<'a, Node>,
    removed: &std::collections::HashSet<NodeId>,
) -> Option<ego_tree::NodeRef<'a, Node>> {
    let mut child = node.first_child();
    while let Some(c) = child {
        if !removed.contains(&c.id()) {
            return Some(c);
        }
        child = c.next_sibling();
    }
    None
}

fn skip_next_sibling<'a>(
    node: &ego_tree::NodeRef<'a, Node>,
    removed: &std::collections::HashSet<NodeId>,
) -> Option<ego_tree::NodeRef<'a, Node>> {
    let mut sib = node.next_sibling();
    while let Some(s) = sib {
        if !removed.contains(&s.id()) {
            return Some(s);
        }
        sib = s.next_sibling();
    }
    None
}

fn td_next<'a>(
    prev: Option<&ego_tree::NodeRef<'a, Node>>,
    current: &ego_tree::NodeRef<'a, Node>,
    removed: &std::collections::HashSet<NodeId>,
) -> Option<ego_tree::NodeRef<'a, Node>> {
    let returning = prev
        .map(|p| p.parent().map(|pp| pp.id()) == Some(current.id()))
        .unwrap_or(false);
    if returning || node_is_pre(current) {
        skip_next_sibling(current, removed).or_else(|| current.parent())
    } else {
        skip_first_child(current, removed)
            .or_else(|| skip_next_sibling(current, removed))
            .or_else(|| current.parent())
    }
}

/// "remove(node)": returns nextSibling || parentNode (no prev update).
fn td_remove_next<'a>(
    node: &ego_tree::NodeRef<'a, Node>,
    removed: &std::collections::HashSet<NodeId>,
) -> Option<ego_tree::NodeRef<'a, Node>> {
    skip_next_sibling(node, removed).or_else(|| node.parent())
}

/// Collapse [ \r\n\t]+ runs to a single space (nbsp is preserved).
fn td_collapse(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut last_was_ws = false;
    for ch in text.chars() {
        if matches!(ch, ' ' | '\r' | '\n' | '\t') {
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

type CollapseResult = (HashMap<NodeId, String>, std::collections::HashSet<NodeId>);

fn collapse_whitespace_pass(root: ElementRef) -> CollapseResult {
    let element: ego_tree::NodeRef<Node> = *root;
    let mut map: HashMap<NodeId, String> = HashMap::new();
    let mut removed: std::collections::HashSet<NodeId> = std::collections::HashSet::new();

    if element.first_child().is_none() || node_is_pre(&element) {
        return (map, removed);
    }

    let mut prev_text: Option<NodeId> = None;
    let mut keep_leading_ws = false;

    let mut prev: Option<ego_tree::NodeRef<Node>> = None;
    let mut node = match td_next(None, &element, &removed) {
        Some(n) => n,
        None => return (map, removed),
    };

    while node.id() != element.id() {
        let mut was_removed = false;
        match node.value() {
            Node::Text(t) => {
                let mut text = td_collapse(&t.text);
                let prev_ends_space = match prev_text {
                    None => true, // !prevText
                    Some(id) => map.get(&id).map(|s| s.ends_with(' ')).unwrap_or(false),
                };
                if prev_ends_space && !keep_leading_ws && text.starts_with(' ') {
                    text.remove(0);
                }
                if text.is_empty() {
                    removed.insert(node.id());
                    was_removed = true;
                } else {
                    map.insert(node.id(), text);
                    prev_text = Some(node.id());
                }
            }
            Node::Element(e) => {
                let tag = e.name().to_lowercase();
                if td_is_block(&tag) || tag == "br" {
                    if let Some(id) = prev_text {
                        if let Some(s) = map.get_mut(&id) {
                            if s.ends_with(' ') {
                                s.pop();
                            }
                        }
                    }
                    prev_text = None;
                    keep_leading_ws = false;
                } else if td_is_void(&tag) || tag == "pre" {
                    // Avoid trimming space around non-block, non-BR void
                    // elements and inline PRE.
                    prev_text = None;
                    keep_leading_ws = true;
                } else if prev_text.is_some() {
                    // Drop protection if set previously.
                    keep_leading_ws = false;
                }
            }
            _ => {
                // Comments, doctypes, PIs: turndown removes them.
                removed.insert(node.id());
                was_removed = true;
            }
        }

        if was_removed {
            node = match td_remove_next(&node, &removed) {
                Some(n) => n,
                None => break,
            };
            continue;
        }

        let next_node = td_next(prev.as_ref(), &node, &removed);
        prev = Some(node);
        node = match next_node {
            Some(n) => n,
            None => break,
        };
    }

    if let Some(id) = prev_text {
        let empty = {
            let s = map.get_mut(&id).unwrap();
            if s.ends_with(' ') {
                s.pop();
            }
            s.is_empty()
        };
        if empty {
            map.remove(&id);
            removed.insert(id);
        }
    }

    (map, removed)
}

// ============================== Internal types ==============================

#[derive(Default)]
struct Ctx {
    list_stack: Vec<ListCtx>,
    in_pre: bool,
    in_code: bool,
    pre_no_code: bool,
    in_heading: bool,
    /// Collapsed text per text-node, produced by the collapseWhitespace
    /// pre-pass (turndown runs this as a mutating DOM pass before any
    /// conversion). Text nodes inside <pre> are never entered and stay raw
    /// (absent from both structures).
    collapsed: HashMap<NodeId, String>,
    /// Text/comment nodes the pre-pass "removed" from the DOM.
    removed: std::collections::HashSet<NodeId>,
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

    for child in parent.children() {
        match child.value() {
            Node::Text(text) => {
                // turndown: replacement = node.isCode ? node.nodeValue : escape(node.nodeValue)
                // nodeValue is the post-collapseWhitespace text: raw inside <pre>
                // (the pre-pass never enters pre), collapsed elsewhere, absent if removed.
                let replacement = if ctx.in_pre {
                    if ctx.pre_no_code {
                        escape_markdown(&text.text)
                    } else {
                        text.text.to_string()
                    }
                } else {
                    match ctx.collapsed.get(&child.id()) {
                        None => continue, // removed by the collapseWhitespace pre-pass
                        Some(collapsed) => {
                            if ctx.in_code {
                                collapsed.clone()
                            } else {
                                escape_markdown(collapsed)
                            }
                        }
                    }
                };
                if !replacement.is_empty() {
                    output = join_str(&output, &replacement);
                }
            }
            Node::Element(_) => {
                if let Some(child_el) = ElementRef::wrap(child) {
                    let tag = child_el.value().name().to_lowercase();
                    if tag == "script" || tag == "style" {
                        continue;
                    }
                    let replacement = replacement_for_node(child_el, ctx);
                    if !replacement.is_empty() {
                        output = join_str(&output, &replacement);
                    }
                }
            }
            _ => {}
        }
    }
    output
}

/// Convert an element node to its Markdown replacement string.
/// Mirrors turndown's replacementForNode: content is computed first via
/// process(), flanking whitespace is extracted for inline nodes (even blank
/// ones — that is how whitespace-only spans contribute their nbsp back), and
/// the rule replacement is wrapped between the flanking edges.
fn replacement_for_node(el: ElementRef, ctx: &mut Ctx) -> String {
    let tag = el.value().name().to_lowercase();

    // Pre-content ctx adjustments (state that must be active while children
    // are processed).
    let saved_pre = ctx.in_pre;
    let saved_code = ctx.in_code;
    let saved_pre_no_code = ctx.pre_no_code;
    match tag.as_str() {
        "pre" => {
            ctx.in_pre = true;
            // turndown: text under pre>code is isCode (raw); other pre text is
            // escaped but never whitespace-collapsed.
            ctx.pre_no_code = true;
        }
        "code" => {
            if ctx.in_pre {
                ctx.pre_no_code = false;
            }
            ctx.in_code = true;
        }
        _ => {}
    }

    let content = process(el, ctx);

    ctx.in_pre = saved_pre;
    ctx.in_code = saved_code;
    ctx.pre_no_code = saved_pre_no_code;

    // Flanking whitespace (turndown: none for block nodes).
    let (leading, trailing) = if td_is_block(&tag) {
        (String::new(), String::new())
    } else {
        flanking_whitespace(el, ctx)
    };
    let content = if !leading.is_empty() || !trailing.is_empty() {
        content.trim().to_string()
    } else {
        content
    };

    let inner = if is_blank_node(el, ctx) {
        // turndown's blankReplacement.
        if td_is_block(&tag) {
            "\n\n".to_string()
        } else {
            String::new()
        }
    } else {
        rule_replacement(&tag, content, el, ctx)
    };

    format!("{leading}{inner}{trailing}")
}

/// Dispatch to the rule for this element. Content has already been computed.
fn rule_replacement(tag: &str, content: String, el: ElementRef, ctx: &mut Ctx) -> String {
    match tag {
        "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => rule_heading(content, tag),
        "p" => format!("\n\n{content}\n\n"),
        "br" => "  \n".to_string(),
        "hr" => "\n\n* * *\n\n".to_string(),
        "strong" | "b" => rule_wrap(content, "**"),
        "em" | "i" => rule_wrap(content, "_"),
        "del" | "s" | "strike" => rule_wrap(content, "~~"),
        "a" => rule_link(content, el),
        "img" => rule_img(el),
        "code" => rule_code(content, el, ctx),
        "pre" => rule_pre(content, el),
        "blockquote" => rule_blockquote(content),
        "ul" | "ol" => rule_list(content, el, ctx),
        "li" => rule_li(content, el, ctx),
        "table" => rule_table(content, el, ctx),
        "thead" | "tbody" | "tfoot" => content,
        "tr" => rule_tr(content, el, ctx),
        "td" | "th" => gfm_cell(&content, el, ctx),
        _ => {
            // turndown's defaultReplacement.
            if td_is_block(tag) {
                format!("\n\n{content}\n\n")
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

// ============================== isBlank & flanking ==============================

/// turndown's isBlank(node), using post-collapse textContent.
fn is_blank_node(el: ElementRef, ctx: &Ctx) -> bool {
    let tag = el.value().name().to_lowercase();
    if td_is_void(&tag) {
        return false;
    }
    if is_meaningful_when_blank(&tag) {
        return false;
    }
    // /^\s*$/i.test(node.textContent)
    let text = collapsed_text_content(el, ctx);
    if !text.chars().all(char::is_whitespace) {
        return false;
    }
    if has_void_descendant(el) || has_meaningful_descendant(el) {
        return false;
    }
    true
}

fn is_meaningful_when_blank(tag: &str) -> bool {
    matches!(
        tag,
        "a" | "table" | "thead" | "tbody" | "tfoot" | "th" | "td"
            | "iframe" | "script" | "audio" | "video"
    )
}

fn has_void_descendant(el: ElementRef) -> bool {
    el.descendants().any(|d| {
        d.value()
            .as_element()
            .is_some_and(|e| td_is_void(&e.name().to_lowercase()))
    })
}

fn has_meaningful_descendant(el: ElementRef) -> bool {
    el.descendants().any(|d| {
        d.value()
            .as_element()
            .is_some_and(|e| is_meaningful_when_blank(&e.name().to_lowercase()))
    })
}

/// turndown's flankingWhitespace(node): edge whitespace of textContent, with
/// ASCII edges dropped when the node is already flanked by whitespace.
fn flanking_whitespace(el: ElementRef, ctx: &Ctx) -> (String, String) {
    let text = collapsed_text_content(el, ctx);
    let edges = edge_whitespace(&text);

    let leading = if !edges.leading_ascii.is_empty() && is_flanked_by_whitespace(el, ctx, false) {
        edges.leading_non_ascii
    } else {
        edges.leading
    };
    let trailing = if !edges.trailing_ascii.is_empty() && is_flanked_by_whitespace(el, ctx, true) {
        edges.trailing_non_ascii
    } else {
        edges.trailing
    };
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

/// turndown's edgeWhitespace regex:
/// ^(([ \t\r\n]*)(\s*))(?:(?=\S)[\s\S]*\S)?((\s*?)([ \t\r\n]*))$
/// For whitespace-only strings the WHOLE string is "leading" and trailing is
/// empty.
fn edge_whitespace(s: &str) -> EdgeWhitespace {
    let chars: Vec<char> = s.chars().collect();
    let len = chars.len();
    let is_ascii_ws = |c: char| matches!(c, ' ' | '\t' | '\r' | '\n');

    let mut i = 0;
    while i < len && is_ascii_ws(chars[i]) {
        i += 1;
    }
    let leading_ascii_end = i;
    while i < len && chars[i].is_whitespace() {
        i += 1;
    }
    let leading_end = i;

    if leading_end == len {
        // Whitespace-only: leading = whole string, trailing empty.
        return EdgeWhitespace {
            leading: s.to_string(),
            leading_ascii: chars[..leading_ascii_end].iter().collect(),
            leading_non_ascii: chars[leading_ascii_end..].iter().collect(),
            trailing: String::new(),
            trailing_ascii: String::new(),
            trailing_non_ascii: String::new(),
        };
    }

    let mut j = len;
    while j > leading_end && is_ascii_ws(chars[j - 1]) {
        j -= 1;
    }
    let trailing_ascii_start = j;
    while j > leading_end && chars[j - 1].is_whitespace() {
        j -= 1;
    }
    let trailing_start = j;

    EdgeWhitespace {
        leading: chars[..leading_end].iter().collect(),
        leading_ascii: chars[..leading_ascii_end].iter().collect(),
        leading_non_ascii: chars[leading_ascii_end..leading_end].iter().collect(),
        trailing: chars[trailing_start..].iter().collect(),
        trailing_ascii: chars[trailing_ascii_start..].iter().collect(),
        trailing_non_ascii: chars[trailing_start..trailing_ascii_start].iter().collect(),
    }
}

/// turndown's isFlankedByWhitespace(side, node): only a plain ASCII space in
/// the adjacent sibling counts. Siblings removed by the pre-pass are skipped.
fn is_flanked_by_whitespace(el: ElementRef, ctx: &Ctx, right: bool) -> bool {
    let mut sibling = if right {
        (*el).next_sibling()
    } else {
        (*el).prev_sibling()
    };
    while let Some(s) = sibling {
        if !is_removed_node(&s, ctx) {
            break;
        }
        sibling = if right { s.next_sibling() } else { s.prev_sibling() };
    }
    let Some(s) = sibling else { return false };
    match s.value() {
        Node::Text(_) => {
            let text = ctx.collapsed.get(&s.id()).cloned().unwrap_or_default();
            if right {
                text.starts_with(' ')
            } else {
                text.ends_with(' ')
            }
        }
        Node::Element(e) => {
            if td_is_block(&e.name().to_lowercase()) {
                return false;
            }
            let Some(se) = ElementRef::wrap(s) else { return false };
            let text = collapsed_text_content(se, ctx);
            if right {
                text.starts_with(' ')
            } else {
                text.ends_with(' ')
            }
        }
        _ => false,
    }
}

// ============================== Rule implementations ==============================

fn rule_heading(content: String, tag: &str) -> String {
    let level: usize = tag[1..].parse().unwrap_or(1);
    let prefix = "#".repeat(level);
    // Project override: unescape "\\." in heading text (src/utils/turndown.ts).
    let cleaned = content.replace("\\.", ".").trim().to_string();
    format!("\n\n{prefix} {cleaned}\n\n")
}

/// strong/em/del: turndown returns '' for whitespace-only content.
fn rule_wrap(content: String, marker: &str) -> String {
    if content.trim().is_empty() {
        return String::new();
    }
    format!("{marker}{content}{marker}")
}

/// turndown's cleanAttribute: collapse newline runs.
fn clean_attribute(value: &str) -> String {
    let re = Regex::new(r"(\n+\s*)+").unwrap();
    re.replace_all(value, "\n").into_owned()
}

fn rule_link(content: String, el: ElementRef) -> String {
    let raw_href = el.value().attr("href").unwrap_or("");
    if raw_href.is_empty() {
        return content;
    }
    let href = escape_url_parens(raw_href);
    let title_part = match el.value().attr("title") {
        Some(t) if !t.is_empty() => {
            let cleaned = clean_attribute(t);
            format!(" \"{}\"", cleaned.replace('"', "\\\""))
        }
        _ => String::new(),
    };
    format!("[{content}]({href}{title_part})")
}

fn rule_img(el: ElementRef) -> String {
    let alt = clean_attribute(el.value().attr("alt").unwrap_or(""));
    let raw_src = el.value().attr("src").unwrap_or("");
    if raw_src.is_empty() {
        return String::new();
    }
    let src = escape_url_parens(raw_src);
    let title_part = match el.value().attr("title") {
        Some(t) if !t.is_empty() => {
            let cleaned = clean_attribute(t);
            format!(" \"{}\"", cleaned.replace('"', "\\\""))
        }
        _ => String::new(),
    };
    format!("![{alt}]({src}{title_part})")
}

/// Inline code. turndown's filter excludes only a CODE that is the sole child
/// of a PRE; a CODE with siblings inside PRE is still inline code.
fn rule_code(content: String, el: ElementRef, ctx: &Ctx) -> String {
    let is_code_block = el
        .parent()
        .and_then(|p| p.value().as_element().map(|e| e.name().eq_ignore_ascii_case("pre")))
        .unwrap_or(false)
        && !has_real_sibling(el, ctx);
    if is_code_block {
        // Handled by the PRE fenced rule; passthrough (defaultReplacement inline).
        return content;
    }
    if content.is_empty() {
        return String::new();
    }
    let content = content.replace(['\r', '\n'], " ");
    let extra_space = if content.starts_with('`')
        || content.ends_with('`')
        || (content.starts_with(' ') && content.ends_with(' ') && content.chars().any(|c| c != ' '))
    {
        " "
    } else {
        ""
    };
    let mut delimiter = "`".to_string();
    let backtick_re = Regex::new(r"`+").unwrap();
    let matches: Vec<&str> = backtick_re.find_iter(&content).map(|m| m.as_str()).collect();
    while matches.contains(&delimiter.as_str()) {
        delimiter.push('`');
    }
    format!("{delimiter}{extra_space}{content}{extra_space}{delimiter}")
}

fn has_real_sibling(el: ElementRef, ctx: &Ctx) -> bool {
    let prev = {
        let mut s = (*el).prev_sibling();
        loop {
            match s {
                None => break false,
                Some(n) if is_removed_node(&n, ctx) => s = n.prev_sibling(),
                Some(_) => break true,
            }
        }
    };
    if prev {
        return true;
    }
    let mut s = (*el).next_sibling();
    loop {
        match s {
            None => break false,
            Some(n) if is_removed_node(&n, ctx) => s = n.next_sibling(),
            Some(_) => break true,
        }
    }
}

fn rule_pre(content: String, el: ElementRef) -> String {
    // turndown fencedCodeBlock filter: PRE whose firstChild is CODE.
    // (pre subtrees are collapseWhitespace sanctuaries, so raw firstChild.)
    let first_child_code = el
        .children()
        .next()
        .and_then(|c| c.value().as_element().map(|e| e.name().eq_ignore_ascii_case("code")))
        .unwrap_or(false);

    if first_child_code {
        let code_el = ElementRef::wrap(el.children().next().unwrap()).unwrap();
        let language = code_el
            .value()
            .attr("class")
            .and_then(|cls| {
                Regex::new(r"language-(\S+)")
                    .unwrap()
                    .captures(cls)
                    .map(|c| c[1].to_string())
            })
            .unwrap_or_default();
        let code: String = code_el.text().collect();

        let mut fence_size = 3usize;
        let fence_re = Regex::new(r"(?m)^`{3,}").unwrap();
        for m in fence_re.find_iter(&code) {
            if m.as_str().len() >= fence_size {
                fence_size = m.as_str().len() + 1;
            }
        }
        let fence: String = "`".repeat(fence_size);
        // code.replace(/\n$/, '') — exactly one trailing newline.
        let code = code.strip_suffix('\n').unwrap_or(&code);
        format!("\n\n{fence}{language}\n{code}\n{fence}\n\n")
    } else {
        // defaultReplacement for a block element.
        format!("\n\n{content}\n\n")
    }
}

/// turndown's blockquote rule: strip edge newlines, then prefix EVERY line
/// with "> " (empty lines become "> " with a trailing space).
fn rule_blockquote(content: String) -> String {
    let trimmed = content.trim_start_matches('\n').trim_end_matches('\n');
    let quoted = trimmed
        .split('\n')
        .map(|line| format!("> {line}"))
        .collect::<Vec<_>>()
        .join("\n");
    format!("\n\n{quoted}\n\n")
}

/// turndown's list rule: '\n' + content when this list is the last element
/// child of an LI, otherwise a block.
fn rule_list(content: String, el: ElementRef, ctx: &Ctx) -> String {
    let _ = ctx;
    let parent_is_li_last = el
        .parent()
        .and_then(ElementRef::wrap)
        .map(|p| {
            p.value().name().eq_ignore_ascii_case("li")
                && p.children()
                    .filter(|c| c.value().is_element())
                    .last()
                    .map(|last| last.id() == (*el).id())
                    .unwrap_or(false)
        })
        .unwrap_or(false);
    if parent_is_li_last {
        format!("\n{content}")
    } else {
        format!("\n\n{content}\n\n")
    }
}

/// Project override of listItem (src/utils/turndown.ts): single space after
/// the marker, ordered lists respect <ol start>.
fn rule_li(content: String, el: ElementRef, ctx: &Ctx) -> String {
    let content = {
        // content.replace(/^\n+/, "").replace(/\n+$/, "\n").replace(/\n/gm, "\n  ")
        let s = content.trim_start_matches('\n');
        let trimmed = s.trim_end_matches('\n');
        let s = if trimmed.len() < s.len() {
            format!("{trimmed}\n")
        } else {
            trimmed.to_string()
        };
        s.replace('\n', "\n  ")
    };

    let parent = el.parent().and_then(ElementRef::wrap);
    let prefix = match &parent {
        Some(p) if p.value().name().eq_ignore_ascii_case("ol") => {
            let start: i64 = p
                .value()
                .attr("start")
                .and_then(|s| s.parse().ok())
                .unwrap_or(1);
            // indexOf among parent.children (element children only).
            let index = p
                .children()
                .filter(|c| c.value().is_element())
                .position(|c| c.id() == (*el).id())
                .unwrap_or(0) as i64;
            format!("{}. ", start + index)
        }
        _ => "- ".to_string(),
    };

    // node.nextSibling (post-cleanup) ? "\n" : ""
    let has_next = {
        let mut s = (*el).next_sibling();
        loop {
            match s {
                None => break false,
                Some(n) if is_removed_node(&n, ctx) => s = n.next_sibling(),
                Some(_) => break true,
            }
        }
    };
    format!("{prefix}{content}{}", if has_next { "\n" } else { "" })
}

// ============================== Table handling ==============================
// Exact port of turndown-plugin-gfm's table rules:
//   tableCell:  cell(content, node) — no trimming, newlines preserved
//   tableRow:   '\n' + content + border cells when heading row
//   table:      convert only when rows[0] is a heading row, else keep raw HTML
//   tableSection: passthrough

/// textContent as turndown sees it post-collapseWhitespace: collapsed map
/// values for text nodes outside <pre>, raw text inside <pre>.
fn collapsed_text_content(el: ElementRef, ctx: &Ctx) -> String {
    let mut out = String::new();
    // If el itself is a <pre>, its whole subtree is a collapse sanctuary.
    let mut in_pre_depth = usize::from(el.value().name().eq_ignore_ascii_case("pre"));
    collect_text(*el, ctx, &mut out, &mut in_pre_depth);
    out
}

fn collect_text(node: ego_tree::NodeRef<Node>, ctx: &Ctx, out: &mut String, pre_depth: &mut usize) {
    for child in node.children() {
        match child.value() {
            Node::Text(t) => {
                if let Some(s) = ctx.collapsed.get(&child.id()) {
                    out.push_str(s);
                } else if *pre_depth > 0 || !ctx.removed.contains(&child.id()) {
                    // Pre-sanctuary text (never visited by the pass) stays raw.
                    out.push_str(&t.text);
                }
            }
            Node::Element(e) => {
                let is_pre = e.name().eq_ignore_ascii_case("pre");
                if is_pre {
                    *pre_depth += 1;
                }
                collect_text(child, ctx, out, pre_depth);
                if is_pre {
                    *pre_depth -= 1;
                }
            }
            _ => {}
        }
    }
}

/// Is this child node "removed" by the collapseWhitespace pre-pass?
/// (Text nodes absent from the map, comments, doctypes.)
fn is_removed_node(child: &ego_tree::NodeRef<Node>, ctx: &Ctx) -> bool {
    match child.value() {
        Node::Text(_) => ctx.removed.contains(&child.id()),
        Node::Element(_) => false,
        _ => true,
    }
}

/// Post-cleanup childNodes (what turndown sees after collapseWhitespace).
fn child_nodes<'a>(el: ElementRef<'a>, ctx: &Ctx) -> Vec<ego_tree::NodeRef<'a, Node>> {
    el.children().filter(|c| !is_removed_node(c, ctx)).collect()
}

fn tag_of(node: &ego_tree::NodeRef<Node>) -> Option<String> {
    node.value().as_element().map(|e| e.name().to_lowercase())
}

/// gfm's cell(content, node): prefix "| " for the first childNode, " " otherwise.
fn gfm_cell(content: &str, el: ElementRef, ctx: &Ctx) -> String {
    let index = el
        .parent()
        .map(|p| {
            ElementRef::wrap(p)
                .map(|pe| {
                    child_nodes(pe, ctx)
                        .iter()
                        .position(|c| c.id() == el.id())
                        .unwrap_or(0)
                })
                .unwrap_or(0)
        })
        .unwrap_or(0);
    let prefix = if index == 0 { "| " } else { " " };
    format!("{prefix}{content} |")
}

/// HTMLTableElement.rows order: thead rows, then direct/tbody rows, then tfoot.
fn table_rows<'a>(table: ElementRef<'a>, ctx: &Ctx) -> Vec<ElementRef<'a>> {
    let mut head: Vec<ElementRef> = Vec::new();
    let mut body: Vec<ElementRef> = Vec::new();
    let mut foot: Vec<ElementRef> = Vec::new();
    for child in child_nodes(table, ctx) {
        let Some(tag) = tag_of(&child) else { continue };
        let Some(el) = ElementRef::wrap(child) else { continue };
        match tag.as_str() {
            "thead" => {
                for c in child_nodes(el, ctx) {
                    if tag_of(&c).as_deref() == Some("tr") {
                        head.push(ElementRef::wrap(c).unwrap());
                    }
                }
            }
            "tfoot" => {
                for c in child_nodes(el, ctx) {
                    if tag_of(&c).as_deref() == Some("tr") {
                        foot.push(ElementRef::wrap(c).unwrap());
                    }
                }
            }
            "tbody" => {
                for c in child_nodes(el, ctx) {
                    if tag_of(&c).as_deref() == Some("tr") {
                        body.push(ElementRef::wrap(c).unwrap());
                    }
                }
            }
            "tr" => body.push(el),
            _ => {}
        }
    }
    head.into_iter().chain(body).chain(foot).collect()
}

/// gfm's isFirstTbody(element).
fn is_first_tbody(el: ElementRef, ctx: &Ctx) -> bool {
    if tag_of(&el).as_deref() != Some("tbody") {
        return false;
    }
    // previousSibling (post-cleanup)
    let mut prev = el.prev_sibling();
    while let Some(p) = prev {
        if !is_removed_node(&p, ctx) {
            break;
        }
        prev = p.prev_sibling();
    }
    match prev {
        None => true,
        Some(p) => {
            if tag_of(&p).as_deref() == Some("thead") {
                let text: String = ElementRef::wrap(p)
                    .map(|pe| collapsed_text_content(pe, ctx))
                    .unwrap_or_default();
                text.trim().is_empty()
            } else {
                false
            }
        }
    }
}

/// gfm's isHeadingRow(tr).
fn is_heading_row(tr: ElementRef, ctx: &Ctx) -> bool {
    let Some(parent) = tr.parent().and_then(ElementRef::wrap) else {
        return false;
    };
    let parent_tag = tag_of(&*parent).unwrap_or_default();
    if parent_tag == "thead" {
        return true;
    }
    let kids = child_nodes(parent, ctx);
    let is_first = kids.first().map(|c| c.id() == tr.id()).unwrap_or(false);
    if !is_first {
        return false;
    }
    if parent_tag != "table" && !is_first_tbody(parent, ctx) {
        return false;
    }
    child_nodes(tr, ctx)
        .iter()
        .all(|c| tag_of(c).as_deref() == Some("th"))
}

fn rule_table(content: String, el: ElementRef, ctx: &Ctx) -> String {
    let rows = table_rows(el, ctx);
    let convertible = rows
        .first()
        .map(|first| is_heading_row(*first, ctx))
        .unwrap_or(false);

    if !convertible {
        // gfm keeps tables without a heading row as raw HTML (block keep).
        // turndown serializes the POST-collapseWhitespace DOM with domino's
        // outerHTML semantics, so we serialize ourselves: removed whitespace
        // text nodes are skipped, collapsed text is used, attributes keep
        // source order, and text/attrs use HTML5 escaping (incl. &nbsp;).
        return format!("\n\n{}\n\n", serialize_kept(el, ctx));
    }

    // gfm: content.replace('\n\n', '\n') — FIRST occurrence only (no /g flag).
    let content = content.replacen("\n\n", "\n", 1);
    format!("\n\n{}\n\n", content)
}

fn rule_tr(content: String, el: ElementRef, ctx: &Ctx) -> String {
    let mut border_cells = String::new();
    if is_heading_row(el, ctx) {
        for child in child_nodes(el, ctx) {
            let Some(cell_el) = ElementRef::wrap(child) else { continue };
            let align = cell_el
                .value()
                .attr("align")
                .map(|a| a.to_lowercase())
                .unwrap_or_default();
            let border = match align.as_str() {
                "left" => ":--",
                "right" => "--:",
                "center" => ":-:",
                _ => "---",
            };
            border_cells.push_str(&gfm_cell(border, cell_el, ctx));
        }
    }
    if border_cells.is_empty() {
        format!("\n{}", content)
    } else {
        format!("\n{}\n{}", content, border_cells)
    }
}

// ============================== Kept-HTML serialization ==============================

fn escape_html_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '\u{a0}' => out.push_str("&nbsp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(c),
        }
    }
    out
}

fn escape_html_attr(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '\u{a0}' => out.push_str("&nbsp;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(c),
        }
    }
    out
}

/// outerHTML of the post-collapse DOM (domino semantics).
fn serialize_kept(el: ElementRef, ctx: &Ctx) -> String {
    let mut out = String::new();
    serialize_node(*el, ctx, &mut out, false);
    out
}

fn serialize_node(node: ego_tree::NodeRef<Node>, ctx: &Ctx, out: &mut String, in_raw: bool) {
    match node.value() {
        Node::Element(e) => {
            let name = e.name();
            out.push('<');
            out.push_str(name);
            for (qual, value) in e.attrs.iter() {
                out.push(' ');
                out.push_str(&qual.local);
                out.push_str("=\"");
                out.push_str(&escape_html_attr(value));
                out.push('"');
            }
            out.push('>');
            if td_is_void(&name.to_lowercase()) {
                return;
            }
            let raw = in_raw || matches!(name.to_lowercase().as_str(), "script" | "style");
            for child in node.children() {
                serialize_node(child, ctx, out, raw);
            }
            out.push_str("</");
            out.push_str(name);
            out.push('>');
        }
        Node::Text(t) => {
            if is_removed_node(&node, ctx) {
                return;
            }
            let text = ctx
                .collapsed
                .get(&node.id())
                .map(|s| s.as_str())
                .unwrap_or(&t.text);
            if in_raw {
                out.push_str(text);
            } else {
                out.push_str(&escape_html_text(text));
            }
        }
        _ => {}
    }
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

/// turndown's escapes array, applied in order. `^` anchors are per-text-node
/// string starts (no multiline flag in turndown).
fn escape_markdown(text: &str) -> String {
    // [/\\/g, '\\\\']
    let mut s = text.replace('\\', "\\\\");
    // [/\*/g, '\\*']
    s = s.replace('*', "\\*");
    // [/^-/g, '\\-']
    if let Some(rest) = s.strip_prefix('-') {
        s = format!("\\-{rest}");
    }
    // [/^\+ /g, '\\+ ']
    if let Some(rest) = s.strip_prefix("+ ") {
        s = format!("\\+ {rest}");
    }
    // [/^(=+)/g, '\\$1']
    if s.starts_with('=') {
        s = format!("\\{s}");
    }
    // [/^(#{1,6}) /g, '\\$1 ']
    let hash_count = s.chars().take_while(|&c| c == '#').count();
    if (1..=6).contains(&hash_count) && s.as_bytes().get(hash_count) == Some(&b' ') {
        s = format!("\\{s}");
    }
    // [/`/g, '\\`']
    s = s.replace('`', "\\`");
    // [/^~~~/g, '\\~~~']
    if s.starts_with("~~~") {
        s = format!("\\{s}");
    }
    // [/\[/g, '\\['] and [/\]/g, '\\]']
    s = s.replace('[', "\\[");
    s = s.replace(']', "\\]");
    // [/^>/g, '\\>']
    if s.starts_with('>') {
        s = format!("\\{s}");
    }
    // [/_/g, '\\_']
    s = s.replace('_', "\\_");
    // [/^(\d+)\. /g, '$1\\. ']
    let digit_count = s.chars().take_while(|c| c.is_ascii_digit()).count();
    if digit_count > 0 && s[digit_count..].starts_with(". ") {
        s = format!("{}\\. {}", &s[..digit_count], &s[digit_count + 2..]);
    }
    s
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
        // turndown prefixes EVERY line with "> " — blank lines keep the
        // trailing space (verified against the TS CLI).
        assert_eq!(
            html_to_markdown(html),
            "> First level\n> \n> > Nested quote"
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
        // turndown's escapes list does NOT include '|' — verified against the
        // TS CLI: createTurndown().turndown("<p>a | b</p>") === "a | b".
        assert_eq!(html_to_markdown("<p>a | b</p>"), "a | b");
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