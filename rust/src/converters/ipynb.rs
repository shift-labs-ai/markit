use anyhow::{Context, Result};
use serde_json::Value;

use crate::types::{ConversionResult, Converter, StreamInfo};

pub struct IpynbConverter;

/// Join a cell's `source` field: either a JSON string or an array of strings.
fn join_source(src: &Value) -> String {
    match src {
        Value::Array(lines) => lines
            .iter()
            .map(|v| v.as_str().unwrap_or(""))
            .collect::<String>(),
        Value::String(s) => s.clone(),
        _ => String::new(),
    }
}

/// Join an array of strings (or a single string) into one string.
fn join_text(val: &Value) -> String {
    match val {
        Value::Array(lines) => lines
            .iter()
            .map(|v| v.as_str().unwrap_or(""))
            .collect::<String>(),
        Value::String(s) => s.clone(),
        _ => String::new(),
    }
}

impl Converter for IpynbConverter {
    fn name(&self) -> &'static str {
        "ipynb"
    }

    fn accepts(&self, info: &StreamInfo) -> bool {
        if let Some(ext) = &info.extension {
            return ext == ".ipynb";
        }
        false
    }

    fn convert(&self, input: &[u8], _info: &StreamInfo) -> Result<ConversionResult> {
        let text = std::str::from_utf8(input).context("ipynb: invalid UTF-8")?;
        let notebook: Value = serde_json::from_str(text).context("ipynb: invalid JSON")?;

        let mut sections: Vec<String> = Vec::new();
        let mut title: Option<String> = None;

        let cells = match notebook.get("cells") {
            Some(Value::Array(c)) => c.as_slice(),
            _ => &[],
        };

        for cell in cells {
            let cell_type = cell.get("cell_type").and_then(Value::as_str).unwrap_or("");
            let source = cell.get("source").map(join_source).unwrap_or_default();

            if cell_type == "markdown" {
                // Extract first H1 heading as title
                if title.is_none() {
                    for line in source.lines() {
                        if let Some(rest) = line.strip_prefix("# ") {
                            let candidate = rest.trim().to_string();
                            if !candidate.is_empty() {
                                title = Some(candidate);
                                break;
                            }
                        }
                    }
                }
                sections.push(source);
            } else if cell_type == "code" {
                // Detect language from kernel metadata
                let lang = notebook
                    .pointer("/metadata/kernelspec/language")
                    .and_then(Value::as_str)
                    .or_else(|| {
                        notebook
                            .pointer("/metadata/language_info/name")
                            .and_then(Value::as_str)
                    })
                    .unwrap_or("python");

                sections.push(format!("```{}\n{}\n```", lang, source));

                // Include text outputs
                let mut outputs: Vec<String> = Vec::new();
                let empty_arr = Value::Array(vec![]);
                let cell_outputs = cell.get("outputs").unwrap_or(&empty_arr);
                if let Value::Array(cell_outputs) = cell_outputs {
                    for out in cell_outputs {
                        let output_type =
                            out.get("output_type").and_then(Value::as_str).unwrap_or("");
                        if output_type == "stream" {
                            let t = out.get("text").map(join_text).unwrap_or_default();
                            let t = t.trim().to_string();
                            if !t.is_empty() {
                                outputs.push(t);
                            }
                        } else if output_type == "execute_result" || output_type == "display_data" {
                            if let Some(data) = out.get("data") {
                                if let Some(plain) = data.get("text/plain") {
                                    let p = join_text(plain);
                                    let p = p.trim().to_string();
                                    if !p.is_empty() {
                                        outputs.push(p);
                                    }
                                }
                            }
                        } else if output_type == "error" {
                            let tb = out
                                .get("traceback")
                                .and_then(Value::as_array)
                                .map(|arr| {
                                    arr.iter()
                                        .map(|v| v.as_str().unwrap_or(""))
                                        .collect::<Vec<_>>()
                                        .join("\n")
                                })
                                .unwrap_or_default();
                            if !tb.trim().is_empty() {
                                let ename = out.get("ename").and_then(Value::as_str).unwrap_or("");
                                let evalue =
                                    out.get("evalue").and_then(Value::as_str).unwrap_or("");
                                outputs.push(format!("Error: {}: {}", ename, evalue));
                            }
                        }
                    }
                }

                if !outputs.is_empty() {
                    sections.push(format!("```\n{}\n```", outputs.join("\n")));
                }
            } else if cell_type == "raw" {
                sections.push(format!("```\n{}\n```", source));
            }
        }

        // Metadata title overrides extracted title
        if let Some(meta_title) = notebook.pointer("/metadata/title").and_then(Value::as_str) {
            title = Some(meta_title.to_string());
        }

        let markdown = sections.join("\n\n").trim().to_string();

        Ok(ConversionResult { markdown, title })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_info(ext: &str) -> StreamInfo {
        StreamInfo {
            extension: Some(ext.to_string()),
            ..Default::default()
        }
    }

    #[test]
    fn accepts_ipynb_extension() {
        let c = IpynbConverter;
        assert!(c.accepts(&make_info(".ipynb")));
    }

    #[test]
    fn rejects_other_extensions() {
        let c = IpynbConverter;
        assert!(!c.accepts(&make_info(".json")));
        assert!(!c.accepts(&make_info(".py")));
        assert!(!c.accepts(&StreamInfo::default()));
    }

    #[test]
    fn empty_notebook() {
        let c = IpynbConverter;
        let nb = r#"{"cells":[],"metadata":{},"nbformat":4,"nbformat_minor":5}"#;
        let res = c.convert(nb.as_bytes(), &make_info(".ipynb")).unwrap();
        assert_eq!(res.markdown, "");
        assert!(res.title.is_none());
    }

    #[test]
    fn markdown_cell_string_source() {
        let c = IpynbConverter;
        let nb = serde_json::json!({
            "cells": [{
                "cell_type": "markdown",
                "source": "# Hello World\nSome text"
            }],
            "metadata": {}
        });
        let res = c
            .convert(nb.to_string().as_bytes(), &make_info(".ipynb"))
            .unwrap();
        assert_eq!(res.markdown, "# Hello World\nSome text");
        assert_eq!(res.title.as_deref(), Some("Hello World"));
    }

    #[test]
    fn markdown_cell_array_source() {
        let c = IpynbConverter;
        let nb = serde_json::json!({
            "cells": [{
                "cell_type": "markdown",
                "source": ["# My Title\n", "second line"]
            }],
            "metadata": {}
        });
        let res = c
            .convert(nb.to_string().as_bytes(), &make_info(".ipynb"))
            .unwrap();
        assert_eq!(res.markdown, "# My Title\nsecond line");
        assert_eq!(res.title.as_deref(), Some("My Title"));
    }

    #[test]
    fn code_cell_default_python() {
        let c = IpynbConverter;
        let nb = serde_json::json!({
            "cells": [{
                "cell_type": "code",
                "source": "print('hello')",
                "outputs": []
            }],
            "metadata": {}
        });
        let res = c
            .convert(nb.to_string().as_bytes(), &make_info(".ipynb"))
            .unwrap();
        assert_eq!(res.markdown, "```python\nprint('hello')\n```");
    }

    #[test]
    fn code_cell_kernelspec_language() {
        let c = IpynbConverter;
        let nb = serde_json::json!({
            "cells": [{
                "cell_type": "code",
                "source": "x <- 1",
                "outputs": []
            }],
            "metadata": {
                "kernelspec": { "language": "R" }
            }
        });
        let res = c
            .convert(nb.to_string().as_bytes(), &make_info(".ipynb"))
            .unwrap();
        assert_eq!(res.markdown, "```R\nx <- 1\n```");
    }

    #[test]
    fn code_cell_language_info_fallback() {
        let c = IpynbConverter;
        let nb = serde_json::json!({
            "cells": [{
                "cell_type": "code",
                "source": "x = 1",
                "outputs": []
            }],
            "metadata": {
                "language_info": { "name": "julia" }
            }
        });
        let res = c
            .convert(nb.to_string().as_bytes(), &make_info(".ipynb"))
            .unwrap();
        assert_eq!(res.markdown, "```julia\nx = 1\n```");
    }

    #[test]
    fn code_cell_stream_output() {
        let c = IpynbConverter;
        let nb = serde_json::json!({
            "cells": [{
                "cell_type": "code",
                "source": "print('hi')",
                "outputs": [{
                    "output_type": "stream",
                    "text": "hi\n"
                }]
            }],
            "metadata": {}
        });
        let res = c
            .convert(nb.to_string().as_bytes(), &make_info(".ipynb"))
            .unwrap();
        assert!(res.markdown.contains("```python\nprint('hi')\n```"));
        assert!(res.markdown.contains("```\nhi\n```"));
    }

    #[test]
    fn code_cell_stream_output_array() {
        let c = IpynbConverter;
        let nb = serde_json::json!({
            "cells": [{
                "cell_type": "code",
                "source": "print(1+1)",
                "outputs": [{
                    "output_type": "stream",
                    "text": ["2", "\n"]
                }]
            }],
            "metadata": {}
        });
        let res = c
            .convert(nb.to_string().as_bytes(), &make_info(".ipynb"))
            .unwrap();
        assert!(res.markdown.contains("```\n2\n```"));
    }

    #[test]
    fn code_cell_execute_result_output() {
        let c = IpynbConverter;
        let nb = serde_json::json!({
            "cells": [{
                "cell_type": "code",
                "source": "1 + 1",
                "outputs": [{
                    "output_type": "execute_result",
                    "data": {
                        "text/plain": "2"
                    }
                }]
            }],
            "metadata": {}
        });
        let res = c
            .convert(nb.to_string().as_bytes(), &make_info(".ipynb"))
            .unwrap();
        assert!(res.markdown.contains("```\n2\n```"));
    }

    #[test]
    fn code_cell_display_data_output() {
        let c = IpynbConverter;
        let nb = serde_json::json!({
            "cells": [{
                "cell_type": "code",
                "source": "df",
                "outputs": [{
                    "output_type": "display_data",
                    "data": {
                        "text/plain": ["a  b\n", "1  2"]
                    }
                }]
            }],
            "metadata": {}
        });
        let res = c
            .convert(nb.to_string().as_bytes(), &make_info(".ipynb"))
            .unwrap();
        assert!(res.markdown.contains("```\na  b\n1  2\n```"));
    }

    #[test]
    fn code_cell_error_output_with_traceback() {
        let c = IpynbConverter;
        let nb = serde_json::json!({
            "cells": [{
                "cell_type": "code",
                "source": "1/0",
                "outputs": [{
                    "output_type": "error",
                    "ename": "ZeroDivisionError",
                    "evalue": "division by zero",
                    "traceback": ["Traceback...", "ZeroDivisionError: division by zero"]
                }]
            }],
            "metadata": {}
        });
        let res = c
            .convert(nb.to_string().as_bytes(), &make_info(".ipynb"))
            .unwrap();
        assert!(res
            .markdown
            .contains("```\nError: ZeroDivisionError: division by zero\n```"));
    }

    #[test]
    fn code_cell_error_empty_traceback_skipped() {
        let c = IpynbConverter;
        let nb = serde_json::json!({
            "cells": [{
                "cell_type": "code",
                "source": "x",
                "outputs": [{
                    "output_type": "error",
                    "ename": "NameError",
                    "evalue": "name 'x' is not defined",
                    "traceback": []
                }]
            }],
            "metadata": {}
        });
        let res = c
            .convert(nb.to_string().as_bytes(), &make_info(".ipynb"))
            .unwrap();
        assert_eq!(res.markdown, "```python\nx\n```");
    }

    #[test]
    fn raw_cell() {
        let c = IpynbConverter;
        let nb = serde_json::json!({
            "cells": [{
                "cell_type": "raw",
                "source": "title: My Notebook\nauthor: Alice"
            }],
            "metadata": {}
        });
        let res = c
            .convert(nb.to_string().as_bytes(), &make_info(".ipynb"))
            .unwrap();
        assert_eq!(res.markdown, "```\ntitle: My Notebook\nauthor: Alice\n```");
    }

    #[test]
    fn metadata_title_overrides_heading() {
        let c = IpynbConverter;
        let nb = serde_json::json!({
            "cells": [{
                "cell_type": "markdown",
                "source": "# Heading Title"
            }],
            "metadata": {
                "title": "Metadata Title"
            }
        });
        let res = c
            .convert(nb.to_string().as_bytes(), &make_info(".ipynb"))
            .unwrap();
        assert_eq!(res.title.as_deref(), Some("Metadata Title"));
    }

    #[test]
    fn multiple_cells_joined_with_double_newline() {
        let c = IpynbConverter;
        let nb = serde_json::json!({
            "cells": [
                { "cell_type": "markdown", "source": "# Intro" },
                { "cell_type": "code", "source": "x = 1", "outputs": [] },
                { "cell_type": "markdown", "source": "## Analysis" }
            ],
            "metadata": {}
        });
        let res = c
            .convert(nb.to_string().as_bytes(), &make_info(".ipynb"))
            .unwrap();
        let parts: Vec<&str> = res.markdown.split("\n\n").collect();
        assert_eq!(parts.len(), 3);
        assert_eq!(parts[0], "# Intro");
        assert_eq!(parts[1], "```python\nx = 1\n```");
        assert_eq!(parts[2], "## Analysis");
    }

    #[test]
    fn empty_stream_output_skipped() {
        let c = IpynbConverter;
        let nb = serde_json::json!({
            "cells": [{
                "cell_type": "code",
                "source": "pass",
                "outputs": [{
                    "output_type": "stream",
                    "text": "   \n"
                }]
            }],
            "metadata": {}
        });
        let res = c
            .convert(nb.to_string().as_bytes(), &make_info(".ipynb"))
            .unwrap();
        assert_eq!(res.markdown, "```python\npass\n```");
    }

    #[test]
    fn missing_cells_field() {
        let c = IpynbConverter;
        let nb = r#"{"metadata":{},"nbformat":4}"#;
        let res = c.convert(nb.as_bytes(), &make_info(".ipynb")).unwrap();
        assert_eq!(res.markdown, "");
    }

    #[test]
    fn invalid_json_returns_error() {
        let c = IpynbConverter;
        let res = c.convert(b"not json", &make_info(".ipynb"));
        assert!(res.is_err());
        assert!(res.unwrap_err().to_string().contains("invalid JSON"));
    }
}
