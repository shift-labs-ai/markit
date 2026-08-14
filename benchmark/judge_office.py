#!/usr/bin/env python3
"""Blind, position-swapped three-way office quality judge.

Uses the Anthropic Messages API and journals every verdict for resumability.
Each document creates three tool pairs, each judged in both A/B orders.
"""

from __future__ import annotations

import argparse
import base64
import itertools
import json
import os
import re
import statistics
import time
import urllib.error
import urllib.request
from collections import defaultdict
from pathlib import Path

TOOLS = ("markit", "anydoc", "liteparse")
DIMENSIONS = ("completeness", "structure", "formatting", "cleanliness")
MODEL = os.environ.get("JUDGE_MODEL", "claude-sonnet-5")
API = "https://api.anthropic.com/v1/messages"
MD_LIMIT = 40_000

PROMPT = """You are judging two Markdown conversions of the same source document.
The source's rendered pages are attached as ground truth. Judge only content
covered by those pages. The deliverable is GitHub-Flavored Markdown; raw HTML
counts against cleanliness.

Score each output from 1 to 5 on completeness, structure, formatting, and
cleanliness. Then choose the overall winner. Reply with ONLY JSON:
{"a":{"completeness":n,"structure":n,"formatting":n,"cleanliness":n},
 "b":{"completeness":n,"structure":n,"formatting":n,"cleanliness":n},
 "winner":"A"|"B"|"tie","reason":"one sentence"}
"""


def clip(text: str) -> str:
    return text if len(text) <= MD_LIMIT else text[:MD_LIMIT] + "\n[truncated]"


def parse_json(text: str) -> dict:
    match = re.search(r"\{.*\}", text, re.DOTALL)
    if not match:
        raise ValueError("judge returned no JSON")
    result = json.loads(match.group())
    if result.get("winner") not in {"A", "B", "tie"}:
        raise ValueError("invalid winner")
    for side in ("a", "b"):
        for dimension in DIMENSIONS:
            value = result[side][dimension]
            if not isinstance(value, int) or not 1 <= value <= 5:
                raise ValueError(f"invalid {side}.{dimension}")
    return result


def request_judge(content: list[dict]) -> dict:
    key = os.environ.get("ANTHROPIC_API_KEY")
    if not key:
        raise RuntimeError("ANTHROPIC_API_KEY is required")
    payload = json.dumps(
        {"model": MODEL, "max_tokens": 1200, "messages": [{"role": "user", "content": content}]}
    ).encode()
    request = urllib.request.Request(
        API,
        data=payload,
        headers={
            "x-api-key": key,
            "anthropic-version": "2023-06-01",
            "content-type": "application/json",
        },
    )
    try:
        with urllib.request.urlopen(request, timeout=300) as response:
            body = json.load(response)
    except urllib.error.HTTPError as error:
        raise RuntimeError(error.read().decode("utf-8", "replace")[:1000]) from error
    text = "".join(block["text"] for block in body["content"] if block["type"] == "text")
    return parse_json(text)


def truth_content(directory: Path, pages: list[Path]) -> list[dict]:
    text_truth = directory / "truth.txt"
    prompt = PROMPT
    if text_truth.exists():
        prompt += "\nGround-truth source text (formatting stripped):\n<ground-truth>\n"
        prompt += text_truth.read_text(encoding="utf-8", errors="replace")
        prompt += "\n</ground-truth>\nJudge structure leniently because textual truth has no formatting."
    content = [{"type": "text", "text": prompt}]
    for page in pages:
        content.append(
            {
                "type": "image",
                "source": {
                    "type": "base64",
                    "media_type": "image/png",
                    "data": base64.b64encode(page.read_bytes()).decode(),
                },
            }
        )
    return content


def load_done(path: Path) -> set[tuple[str, str, str]]:
    done = set()
    if path.exists():
        for line in path.read_text().splitlines():
            row = json.loads(line)
            done.add((row["file"], row["a"], row["b"]))
    return done


def summarize(journal: Path, output: Path) -> None:
    rows = [json.loads(line) for line in journal.read_text().splitlines() if line.strip()]
    scores = defaultdict(lambda: defaultdict(list))
    for row in rows:
        for side in ("a", "b"):
            tool = row[side]
            rubric = row["verdict"][side]
            for dimension in DIMENSIONS:
                scores[tool][dimension].append(rubric[dimension])
    summary = {}
    for tool in TOOLS:
        dimensions = {
            dimension: round(statistics.fmean(values) / 5 * 100, 1) if values else 0
            for dimension, values in scores[tool].items()
        }
        summary[tool] = {
            "score": round(statistics.fmean(dimensions.values()), 1) if dimensions else 0,
            **dimensions,
            "judgments": len(next(iter(scores[tool].values()), [])),
        }
    (output / "quality.json").write_text(json.dumps({"model": MODEL, "tools": summary}, indent=2) + "\n")
    lines = [
        "# Office quality",
        "",
        f"Blind, position-swapped visual judge: `{MODEL}`.",
        "",
        "| Tool | Score | Completeness | Structure | Formatting | Cleanliness | Judgments |",
        "| --- | ---: | ---: | ---: | ---: | ---: | ---: |",
    ]
    for tool in TOOLS:
        row = summary[tool]
        lines.append(
            f"| {tool} | {row['score']:.1f} | {row.get('completeness', 0):.1f} | "
            f"{row.get('structure', 0):.1f} | {row.get('formatting', 0):.1f} | "
            f"{row.get('cleanliness', 0):.1f} | {row['judgments']} |"
        )
    (output / "quality.md").write_text("\n".join(lines) + "\n")
    print("\n".join(lines))


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("results", type=Path, help="benchmark/results/office")
    parser.add_argument("--truth", type=Path)
    parser.add_argument("--limit", type=int, default=0)
    parser.add_argument("--dry-run", action="store_true")
    args = parser.parse_args()
    truth = args.truth or args.results / "truth"
    journal = args.results / "judge.jsonl"
    done = load_done(journal)

    tasks = []
    documents = sorted(path for path in truth.iterdir() if path.is_dir())
    if args.limit:
        documents = documents[: args.limit]
    for document in documents:
        pages = sorted(document.glob("page-*.png"))
        available = [tool for tool in TOOLS if (args.results / tool / f"{document.name}.md").exists()]
        for left, right in itertools.combinations(available, 2):
            for a, b in ((left, right), (right, left)):
                if (document.name, a, b) not in done:
                    tasks.append((document.name, pages, a, b))
    print(f"{len(tasks)} judge calls pending; model {MODEL}")
    if args.dry_run:
        return 0

    for index, (filename, pages, a, b) in enumerate(tasks, 1):
        content = truth_content(truth / filename, pages)
        md_a = (args.results / a / f"{filename}.md").read_text(errors="replace")
        md_b = (args.results / b / f"{filename}.md").read_text(errors="replace")
        content.append({"type": "text", "text": f"<output-A>\n{clip(md_a)}\n</output-A>\n\n<output-B>\n{clip(md_b)}\n</output-B>"})
        verdict = request_judge(content)
        row = {"file": filename, "a": a, "b": b, "verdict": verdict, "model": MODEL, "timestamp": time.time()}
        with journal.open("a") as stream:
            stream.write(json.dumps(row) + "\n")
        print(f"[{index}/{len(tasks)}] {filename}: {a} vs {b} -> {verdict['winner']}")

    summarize(journal, args.results)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
