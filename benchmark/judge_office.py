#!/usr/bin/env python3
"""Blind, position-swapped three-way office quality judge.

Uses OpenAI's Responses API with structured output. Every task is addressed by
its model, prompt, truth, and candidate hashes, so interrupted runs resume while
changed inputs are automatically re-judged.
"""

from __future__ import annotations

import argparse
import base64
import hashlib
import itertools
import json
import os
import statistics
import time
import urllib.error
import urllib.request
from collections import defaultdict
from pathlib import Path

TOOLS = ("markit", "anydoc", "liteparse")
DIMENSIONS = ("completeness", "structure", "formatting", "cleanliness")
MODEL = os.environ.get("JUDGE_MODEL", "gpt-5.6-terra")
REASONING_EFFORT = os.environ.get("JUDGE_REASONING_EFFORT", "low")
API = "https://api.openai.com/v1/responses"
MD_LIMIT = 40_000

PROMPT = """You are judging two Markdown conversions of the same source document.
Judge only source content covered by the supplied ground truth. The deliverable
is GitHub-Flavored Markdown; content present only as raw HTML counts against
cleanliness.

Score each output from 1 to 5 on:
- completeness: source content is present and nothing is invented
- structure: headings, lists, reading order, and tables match the source
- formatting: emphasis, links, footnotes, and other formatting are faithful
- cleanliness: no garbage, broken escaping, raw HTML, or extraction artifacts

Choose the overall winner after scoring both outputs. Evaluate A and B
independently; do not reward verbosity or output length by itself.
"""

VERDICT_SCHEMA = {
    "type": "object",
    "properties": {
        "a": {
            "type": "object",
            "properties": {dimension: {"type": "integer", "minimum": 1, "maximum": 5} for dimension in DIMENSIONS},
            "required": list(DIMENSIONS),
            "additionalProperties": False,
        },
        "b": {
            "type": "object",
            "properties": {dimension: {"type": "integer", "minimum": 1, "maximum": 5} for dimension in DIMENSIONS},
            "required": list(DIMENSIONS),
            "additionalProperties": False,
        },
        "winner": {"type": "string", "enum": ["A", "B", "tie"]},
        "reason": {"type": "string"},
    },
    "required": ["a", "b", "winner", "reason"],
    "additionalProperties": False,
}


def digest_bytes(value: bytes) -> str:
    return hashlib.sha256(value).hexdigest()


def digest_file(path: Path) -> str:
    return digest_bytes(path.read_bytes())


def clip(text: str) -> str:
    return text if len(text) <= MD_LIMIT else text[:MD_LIMIT] + "\n[truncated]"


def task_id(filename: str, a: str, b: str, truth_hash: str, a_hash: str, b_hash: str) -> str:
    identity = {
        "file": filename,
        "a": a,
        "b": b,
        "truth": truth_hash,
        "a_output": a_hash,
        "b_output": b_hash,
        "model": MODEL,
        "reasoning_effort": REASONING_EFFORT,
        "prompt": digest_bytes(PROMPT.encode()),
    }
    return digest_bytes(json.dumps(identity, sort_keys=True).encode())


def parse_verdict(result: dict) -> dict:
    output_text = []
    for item in result.get("output", []):
        if item.get("type") != "message":
            continue
        for content in item.get("content", []):
            if content.get("type") == "output_text":
                output_text.append(content["text"])
    if not output_text:
        raise ValueError("judge returned no output_text")
    verdict = json.loads("".join(output_text))
    if verdict.get("winner") not in {"A", "B", "tie"}:
        raise ValueError("judge returned an invalid winner")
    for side in ("a", "b"):
        for dimension in DIMENSIONS:
            value = verdict[side][dimension]
            if not isinstance(value, int) or not 1 <= value <= 5:
                raise ValueError(f"invalid {side}.{dimension}")
    return verdict


def request_judge(content: list[dict], retries: int = 5) -> dict:
    key = os.environ.get("OPENAI_API_KEY")
    if not key:
        raise RuntimeError("OPENAI_API_KEY is required")
    payload = json.dumps(
        {
            "model": MODEL,
            "reasoning": {"effort": REASONING_EFFORT},
            "max_output_tokens": 1200,
            "input": [{"role": "user", "content": content}],
            "text": {
                "format": {
                    "type": "json_schema",
                    "name": "document_conversion_verdict",
                    "strict": True,
                    "schema": VERDICT_SCHEMA,
                }
            },
        }
    ).encode()
    request = urllib.request.Request(
        API,
        data=payload,
        headers={
            "authorization": f"Bearer {key}",
            "content-type": "application/json",
        },
    )
    for attempt in range(retries):
        try:
            with urllib.request.urlopen(request, timeout=300) as response:
                return parse_verdict(json.load(response))
        except urllib.error.HTTPError as error:
            body = error.read().decode("utf-8", "replace")[:1000]
            if error.code not in {408, 409, 429, 500, 502, 503, 504} or attempt + 1 == retries:
                raise RuntimeError(f"OpenAI API {error.code}: {body}") from error
        except urllib.error.URLError as error:
            if attempt + 1 == retries:
                raise RuntimeError(f"OpenAI API connection failed: {error}") from error
        time.sleep(2**attempt)
    raise AssertionError("retry loop exited unexpectedly")


def truth_payload(directory: Path) -> tuple[list[dict], str]:
    text_truth = directory / "truth.txt"
    pages = sorted(directory.glob("page-*.png"))
    if text_truth.exists():
        text = text_truth.read_text(encoding="utf-8", errors="replace")
        prompt = (
            PROMPT
            + "\nGround truth is source text with formatting stripped. Judge structure and formatting leniently."
            + f"\n<ground-truth>\n{text}\n</ground-truth>"
        )
        return [{"type": "input_text", "text": prompt}], digest_file(text_truth)
    if not pages:
        raise RuntimeError(f"no truth pages or truth.txt in {directory}")
    content = [
        {
            "type": "input_text",
            "text": PROMPT + f"\nGround truth is attached as {len(pages)} rendered page image(s).",
        }
    ]
    hashes = []
    for page in pages:
        image = page.read_bytes()
        hashes.append(digest_bytes(image))
        content.append(
            {
                "type": "input_image",
                "image_url": "data:image/png;base64," + base64.b64encode(image).decode(),
                "detail": "high",
            }
        )
    return content, digest_bytes("\n".join(hashes).encode())


def load_journal(path: Path) -> dict[str, dict]:
    records = {}
    if path.exists():
        for line in path.read_text(encoding="utf-8").splitlines():
            if line.strip():
                record = json.loads(line)
                records[record["task_id"]] = record
    return records


def build_tasks(results: Path, truth: Path, limit: int) -> list[dict]:
    tasks = []
    documents = sorted(path for path in truth.iterdir() if path.is_dir())
    if limit:
        documents = documents[:limit]
    for document in documents:
        base_content, truth_hash = truth_payload(document)
        outputs = {
            tool: results / tool / f"{document.name}.md"
            for tool in TOOLS
            if (results / tool / f"{document.name}.md").exists()
        }
        for left, right in itertools.combinations(outputs, 2):
            for a, b in ((left, right), (right, left)):
                a_hash, b_hash = digest_file(outputs[a]), digest_file(outputs[b])
                tasks.append(
                    {
                        "task_id": task_id(document.name, a, b, truth_hash, a_hash, b_hash),
                        "file": document.name,
                        "format": Path(document.name).suffix.lstrip(".").lower(),
                        "a": a,
                        "b": b,
                        "a_path": outputs[a],
                        "b_path": outputs[b],
                        "truth_content": base_content,
                        "truth_sha256": truth_hash,
                        "a_output_sha256": a_hash,
                        "b_output_sha256": b_hash,
                    }
                )
    return tasks


def summarize(records: dict[str, dict], valid_task_ids: set[str], output: Path) -> None:
    scores = defaultdict(lambda: defaultdict(lambda: defaultdict(list)))
    docs = defaultdict(set)
    for task_key in valid_task_ids:
        row = records.get(task_key)
        if not row:
            continue
        for side in ("a", "b"):
            tool = row[side]
            rubric = row["verdict"][side]
            docs[tool].add(row["file"])
            for dimension in DIMENSIONS:
                scores[tool][row["format"]][dimension].append(rubric[dimension])

    summary = {}
    for tool in TOOLS:
        formats = scores[tool]
        per_format = {}
        for format_name, dimensions in formats.items():
            values = {
                dimension: statistics.fmean(dimensions[dimension]) / 5 * 100
                for dimension in DIMENSIONS
            }
            per_format[format_name] = {
                "score": round(statistics.fmean(values.values()), 1),
                **{key: round(value, 1) for key, value in values.items()},
            }
        macro_dimensions = {
            dimension: round(
                statistics.fmean(
                    statistics.fmean(values[dimension]) / 5 * 100
                    for values in formats.values()
                ),
                1,
            )
            if formats
            else 0
            for dimension in DIMENSIONS
        }
        summary[tool] = {
            "score": round(statistics.fmean(macro_dimensions.values()), 1)
            if formats
            else 0,
            **macro_dimensions,
            "formats": len(formats),
            "documents": len(docs[tool]),
            "by_format": per_format,
        }

    result = {
        "provider": "openai",
        "model": MODEL,
        "reasoning_effort": REASONING_EFFORT,
        "prompt_sha256": digest_bytes(PROMPT.encode()),
        "tools": summary,
    }
    (output / "quality.json").write_text(json.dumps(result, indent=2) + "\n")
    lines = [
        "# Office quality",
        "",
        f"Blind, position-swapped visual judge: `{MODEL}` ({REASONING_EFFORT} reasoning).",
        "Scores are macro-averaged across each tool's judged formats.",
        "",
        "| Tool | Score | Completeness | Structure | Formatting | Cleanliness | Formats | Docs |",
        "| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |",
    ]
    for tool in TOOLS:
        row = summary[tool]
        lines.append(
            f"| {tool} | {row['score']:.1f} | {row['completeness']:.1f} | "
            f"{row['structure']:.1f} | {row['formatting']:.1f} | "
            f"{row['cleanliness']:.1f} | {row['formats']} | {row['documents']} |"
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
    if not truth.is_dir():
        parser.error(f"truth directory does not exist: {truth}")

    journal = args.results / "judge.jsonl"
    records = load_journal(journal)
    tasks = build_tasks(args.results, truth, args.limit)
    pending = [task for task in tasks if task["task_id"] not in records]
    print(f"{len(pending)} judge calls pending of {len(tasks)}; model {MODEL}")
    if args.dry_run:
        return 0

    for index, task in enumerate(pending, 1):
        md_a = task["a_path"].read_text(encoding="utf-8", errors="replace")
        md_b = task["b_path"].read_text(encoding="utf-8", errors="replace")
        content = list(task["truth_content"])
        content.append(
            {
                "type": "input_text",
                "text": f"<output-A>\n{clip(md_a)}\n</output-A>\n\n<output-B>\n{clip(md_b)}\n</output-B>",
            }
        )
        verdict = request_judge(content)
        record = {
            key: value
            for key, value in task.items()
            if key not in {"a_path", "b_path", "truth_content"}
        }
        record.update(
            verdict=verdict,
            provider="openai",
            model=MODEL,
            reasoning_effort=REASONING_EFFORT,
            timestamp=time.time(),
        )
        with journal.open("a", encoding="utf-8") as stream:
            stream.write(json.dumps(record) + "\n")
        records[task["task_id"]] = record
        print(
            f"[{index}/{len(pending)}] {task['file']}: "
            f"{task['a']} vs {task['b']} -> {verdict['winner']}"
        )

    summarize(records, {task["task_id"] for task in tasks}, args.results)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
