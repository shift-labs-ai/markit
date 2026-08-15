#!/usr/bin/env python3
"""Score shitty-pdf-bench Markdown outputs against curated golden assertions."""

from __future__ import annotations

import argparse
import html
import json
import re
from pathlib import Path

TOOLS = ("markit", "anydoc", "liteparse")
WORD_PATTERN = re.compile(r"[^\W_]+(?:['’][^\W_]+)?|\d+(?:[.,]\d+)*", re.UNICODE)


def normalized(text: str) -> str:
    text = text.replace("®", "").replace("™", "")
    return " ".join(text.casefold().split())


def normalized_words(text: str) -> str:
    text = html.unescape(text)
    text = re.sub(r"(?<=\w)-(?:<br\s*/?>|\s)+(?=\w)", "", text, flags=re.IGNORECASE)
    text = re.sub(r"([A-Za-z])_\{\s*([A-Za-z]+)\s*\}", r"\1\2", text)
    text = re.sub(r"\\(?:times|cdot)\b", " ", text)
    text = re.sub(
        r"\\langle\s*([A-Za-z]+)\\rangle",
        lambda match: f"h{match.group(1)}i",
        text,
    )
    return " ".join(WORD_PATTERN.findall(text.casefold()))


def evaluate(markdown: str, check: dict) -> bool:
    kind = check["type"]
    value = check["value"]
    if kind == "contains":
        return normalized(str(value)) in normalized(markdown)
    if kind == "contains_words":
        return normalized_words(str(value)) in normalized_words(markdown)
    if kind == "not_contains":
        return normalized(str(value)) not in normalized(markdown)
    if kind == "regex":
        return re.search(str(value), markdown, re.IGNORECASE | re.MULTILINE) is not None
    if kind == "min_chars":
        return len(markdown) >= int(value)
    if kind == "min_table_rows":
        rows = sum(
            1
            for line in markdown.splitlines()
            if line.strip().startswith("|") and line.strip().endswith("|")
        )
        return rows >= int(value)
    raise ValueError(f"unknown check type: {kind}")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "results", type=Path, help="benchmark/results/shitty-pdf-bench directory"
    )
    parser.add_argument(
        "--assertions",
        type=Path,
        default=Path(__file__).with_name("shitty-pdf-bench-assertions.json"),
    )
    parser.add_argument("--output-stem", default="quality")
    parser.add_argument("--label", default="shitty-pdf-bench quality")
    args = parser.parse_args()

    spec = json.loads(args.assertions.read_text())
    details = []
    for tool in TOOLS:
        for filename, document in spec["documents"].items():
            if not document["checks"]:
                continue
            output = args.results / tool / f"{filename}.md"
            if not output.exists():
                details.append(
                    {"tool": tool, "file": filename, "passed": 0, "total": len(document["checks"]), "missing": True}
                )
                continue
            markdown = output.read_text(encoding="utf-8", errors="replace")
            checks = []
            for check in document["checks"]:
                checks.append({**check, "passed": evaluate(markdown, check)})
            details.append(
                {
                    "tool": tool,
                    "file": filename,
                    "passed": sum(check["passed"] for check in checks),
                    "total": len(checks),
                    "checks": checks,
                }
            )

    summary = {}
    for tool in TOOLS:
        rows = [row for row in details if row["tool"] == tool]
        passed = sum(row["passed"] for row in rows)
        total = sum(row["total"] for row in rows)
        summary[tool] = {
            "passed": passed,
            "total": total,
            "score": round(100 * passed / total, 1) if total else 0,
            "documents_complete": sum(row["passed"] == row["total"] for row in rows),
            "documents": len(rows),
        }

    result = {"schema": 1, "summary": summary, "details": details}
    destination = args.results / f"{args.output_stem}.json"
    destination.write_text(json.dumps(result, indent=2) + "\n")

    lines = [
        f"# {args.label}",
        "",
        "| Tool | Checks | Score | Documents passing every check |",
        "| --- | ---: | ---: | ---: |",
    ]
    for tool in TOOLS:
        row = summary[tool]
        lines.append(
            f"| {tool} | {row['passed']}/{row['total']} | {row['score']:.1f}% | "
            f"{row['documents_complete']}/{row['documents']} |"
        )
    lines.append("")
    (args.results / f"{args.output_stem}.md").write_text("\n".join(lines))
    print("\n".join(lines))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
