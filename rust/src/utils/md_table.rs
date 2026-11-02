//! GFM pipe tables: cell escaping and table layout shared by every
//! converter that emits a markdown table.

/// Make `text` safe inside one GFM table cell: every `|` becomes `\|`,
/// and the line breaks that would end the row become `<br>`. Each line
/// is trimmed and blank lines are dropped, so a paragraph break or a
/// markdown hard break (`"  \n"`) yields a single `<br>`.
pub fn escape_cell(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("<br>")
        .replace('|', "\\|")
}

/// Render `rows` as a GFM table with the first row as its header. Every
/// row is padded to the widest one and every cell goes through
/// [`escape_cell`]. No rows, or no columns, render as an empty string.
pub fn render(rows: &[Vec<String>]) -> String {
    let width = rows.iter().map(Vec::len).max().unwrap_or(0);
    if width == 0 {
        return String::new();
    }
    let line = |cells: Vec<String>| format!("| {} |", cells.join(" | "));
    let row = |cells: &Vec<String>| {
        line(
            (0..width)
                .map(|i| cells.get(i).map_or_else(String::new, |c| escape_cell(c)))
                .collect(),
        )
    };
    let mut lines = vec![row(&rows[0]), line(vec!["---".to_string(); width])];
    lines.extend(rows[1..].iter().map(row));
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(rows: &[&[&str]]) -> Vec<Vec<String>> {
        rows.iter()
            .map(|r| r.iter().map(|c| c.to_string()).collect())
            .collect()
    }

    #[test]
    fn pipes_are_escaped() {
        assert_eq!(escape_cell("a|b"), "a\\|b");
        assert_eq!(escape_cell("`x || y`"), "`x \\|\\| y`");
    }

    #[test]
    fn line_breaks_become_br() {
        assert_eq!(escape_cell("one\ntwo"), "one<br>two");
        assert_eq!(escape_cell("one\r\ntwo"), "one<br>two");
        assert_eq!(escape_cell("\n\none  \n\n  two\n\n"), "one<br>two");
    }

    #[test]
    fn renders_header_divider_and_padded_body() {
        let table = render(&rows(&[&["A", "B|C"], &["1"], &["2", "x\ny", "extra"]]));
        assert_eq!(
            table,
            "| A | B\\|C |  |\n| --- | --- | --- |\n| 1 |  |  |\n| 2 | x<br>y | extra |"
        );
    }

    #[test]
    fn header_only_and_empty() {
        assert_eq!(render(&rows(&[&["A"]])), "| A |\n| --- |");
        assert_eq!(render(&[]), "");
        assert_eq!(render(&[vec![]]), "");
    }
}
