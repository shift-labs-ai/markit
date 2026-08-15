#!/usr/bin/env python3
"""Aggregate official olmOCR scorer logs and conversion timings."""

import argparse
import json
import re
import statistics
from pathlib import Path

TOOLS = ("markit", "anydoc", "liteparse")


def parse_score(path: Path) -> dict:
    text = path.read_text(encoding="utf-8", errors="replace")
    summary = re.search(
        r"^([^\n:]+): Average Score: ([\d.]+)% ± ([\d.]+)%", text, re.MULTILINE
    )
    if not summary:
        return {"error": "official score not found"}
    categories = {}
    for name, score, passed, total in re.findall(
        r"^\s+([a-z_]+)\.jsonl\s+:\s+([\d.]+)% \((\d+)/(\d+) tests\)",
        text,
        re.MULTILINE,
    ):
        categories[name] = {
            "score": float(score),
            "passed": int(passed),
            "total": int(total),
        }
    return {
        "score": float(summary.group(2)),
        "ci95": float(summary.group(3)),
        "categories": categories,
    }


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("results", type=Path)
    args = parser.parse_args()
    timing_path = args.results / "timings.json"
    timings = json.loads(timing_path.read_text()) if timing_path.exists() else []
    result = {"tools": {}}
    for tool in TOOLS:
        rows = [row for row in timings if row["tool"] == tool]
        successes = [row["ms"] for row in rows if row.get("ok") and "ms" in row]
        result["tools"][tool] = {
            **parse_score(args.results / f"{tool}.txt"),
            "converted": sum(bool(row.get("ok")) for row in rows),
            "failures": sum(not row.get("ok") for row in rows),
            "median_cli_ms": round(statistics.median(successes), 3) if successes else None,
            "total_cli_seconds": round(sum(successes) / 1000, 3) if successes else None,
        }
    (args.results / "quality.json").write_text(json.dumps(result, indent=2) + "\n")
    lines = [
        "# olmOCR-bench: three-way non-OCR PDF benchmark",
        "",
        "Official 8,413-check evaluator; Liteparse OCR disabled.",
        "",
        "| Tool | Quality | 95% CI | Converted | Failures | Median CLI ms |",
        "| --- | ---: | ---: | ---: | ---: | ---: |",
    ]
    for tool in TOOLS:
        row = result["tools"][tool]
        lines.append(
            f"| {tool} | {row.get('score', 0):.1f}% | ±{row.get('ci95', 0):.1f}% | "
            f"{row['converted']} | {row['failures']} | "
            f"{row['median_cli_ms'] if row['median_cli_ms'] is not None else '-'} |"
        )
    lines.append("")
    (args.results / "report.md").write_text("\n".join(lines))
    print("\n".join(lines))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
