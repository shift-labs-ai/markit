#!/usr/bin/env python3
"""Three-way benchmark runner for markit, anydoc, and liteparse.

Runs each converter as an end-to-end CLI, preserves raw Markdown, and records
wall-clock timings plus provenance in JSON. Quality is scored separately by the
suite's canonical evaluator.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import platform
import shutil
import statistics
import subprocess
import sys
import tempfile
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
BENCH = ROOT / "benchmark"
TOOLS = ("markit", "anydoc", "liteparse")
OFFICE_EXTENSIONS = {".docx", ".pptx", ".xlsx", ".epub", ".csv"}


def executable(env: str, candidates: list[str | Path]) -> str:
    override = os.environ.get(env)
    if override:
        return override
    for candidate in candidates:
        path = shutil.which(str(candidate))
        if path:
            return path
        if Path(candidate).is_file():
            return str(Path(candidate).resolve())
    raise RuntimeError(f"{env} is not set and no executable was found: {candidates}")


def tool_bins() -> dict[str, str]:
    return {
        "markit": executable(
            "MARKIT_BIN",
            [ROOT / "rust" / "target" / "release" / "markit", "markit"],
        ),
        "anydoc": executable("ANYDOC_BIN", ["anydoc"]),
        "liteparse": executable("LITEPARSE_BIN", ["lit"]),
    }


def version(tool: str, binary: str) -> str:
    flags = ["--version", "-V"]
    for flag in flags:
        result = subprocess.run([binary, flag], capture_output=True, text=True)
        text = (result.stdout or result.stderr).strip()
        if result.returncode == 0 and text:
            return text.splitlines()[0]
    return "unknown"


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def run_once(tool: str, binary: str, source: Path) -> tuple[str, float]:
    with tempfile.TemporaryDirectory(prefix="markit-bench-") as temp:
        output = Path(temp) / "output.md"
        if tool == "markit":
            command = [binary, str(source), "--quiet"]
        elif tool == "anydoc":
            command = [binary, str(source)]
        else:
            command = [
                binary,
                "parse",
                str(source),
                "--format",
                "markdown",
                "--no-ocr",
                "--image-mode",
                "off",
                "--output",
                str(output),
            ]

        started = time.perf_counter_ns()
        result = subprocess.run(command, capture_output=True)
        elapsed_ms = (time.perf_counter_ns() - started) / 1_000_000
        if result.returncode != 0:
            error = result.stderr.decode("utf-8", "replace").strip()
            raise RuntimeError(error[:1000] or f"exit {result.returncode}")
        markdown = (
            output.read_text(encoding="utf-8", errors="replace")
            if output.exists()
            else result.stdout.decode("utf-8", "replace")
        )
        return markdown, elapsed_ms


def corpus_files(suite: str, corpus: Path) -> list[Path]:
    files = sorted(path for path in corpus.iterdir() if path.is_file())
    if suite == "office":
        files = [path for path in files if path.suffix.lower() in OFFICE_EXTENSIONS]
    elif suite == "horrible":
        files = [path for path in files if path.suffix.lower() == ".pdf"]
    if not files:
        raise RuntimeError(f"no {suite} files found in {corpus}")
    return files


def safe_name(path: Path) -> str:
    return path.name.replace("/", "_") + ".md"


def report(rows: list[dict], output: Path, suite: str) -> None:
    lines = [
        f"# {suite.title()} benchmark: markit vs anydoc vs liteparse",
        "",
        "End-to-end warm CLI wall time. PDF runs disable Liteparse OCR.",
        "",
        "| Tool | Passed | Failed | Median ms/doc | Total seconds |",
        "| --- | ---: | ---: | ---: | ---: |",
    ]
    for tool in TOOLS:
        selected = [row for row in rows if row["tool"] == tool]
        passed = [row for row in selected if row["ok"]]
        times = [row["median_ms"] for row in passed]
        median = f"{statistics.median(times):.2f}" if times else "-"
        total = f"{sum(times) / 1000:.2f}" if times else "-"
        lines.append(
            f"| {tool} | {len(passed)} | {len(selected) - len(passed)} | {median} | {total} |"
        )
    lines += ["", "Quality is reported by the suite-specific scorer; timing is not a quality score.", ""]
    (output / "report.md").write_text("\n".join(lines), encoding="utf-8")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("suite", choices=("office", "horrible"))
    parser.add_argument("--corpus", type=Path)
    parser.add_argument("--output", type=Path)
    parser.add_argument("--tools", default=",".join(TOOLS))
    parser.add_argument("--warmups", type=int, default=1)
    parser.add_argument("--iterations", type=int, default=3)
    args = parser.parse_args()

    if args.corpus:
        corpus = args.corpus
    elif args.suite == "office":
        corpus = BENCH / "corpus"
    elif os.environ.get("HORRIBLE_PDF_DIR"):
        corpus = Path(os.environ["HORRIBLE_PDF_DIR"])
    else:
        parser.error("--corpus or HORRIBLE_PDF_DIR is required for horrible")
    output = args.output or BENCH / "results" / args.suite
    output.mkdir(parents=True, exist_ok=True)

    selected_tools = [tool.strip() for tool in args.tools.split(",") if tool.strip()]
    unknown = set(selected_tools) - set(TOOLS)
    if unknown:
        parser.error(f"unknown tools: {sorted(unknown)}")

    bins = tool_bins()
    provenance = {
        "suite": args.suite,
        "timestamp": time.time(),
        "platform": platform.platform(),
        "machine": platform.machine(),
        "python": platform.python_version(),
        "warmups": args.warmups,
        "iterations": args.iterations,
        "tools": {
            tool: {"binary": bins[tool], "version": version(tool, bins[tool])}
            for tool in selected_tools
        },
    }
    (output / "provenance.json").write_text(json.dumps(provenance, indent=2) + "\n")

    rows: list[dict] = []
    files = corpus_files(args.suite, corpus)
    for file_index, source in enumerate(files):
        # Rotate order per file to reduce systematic thermal/order bias.
        ordered = selected_tools[file_index % len(selected_tools) :] + selected_tools[: file_index % len(selected_tools)]
        for tool in ordered:
            row = {
                "suite": args.suite,
                "tool": tool,
                "file": source.name,
                "format": source.suffix.lower().lstrip("."),
                "bytes": source.stat().st_size,
                "sha256": sha256(source),
            }
            try:
                for _ in range(args.warmups):
                    run_once(tool, bins[tool], source)
                timings = []
                markdown = ""
                output_hashes = []
                for _ in range(args.iterations):
                    markdown, elapsed = run_once(tool, bins[tool], source)
                    timings.append(elapsed)
                    output_hashes.append(hashlib.sha256(markdown.encode()).hexdigest())
                tool_dir = output / tool
                tool_dir.mkdir(exist_ok=True)
                destination = tool_dir / safe_name(source)
                destination.write_text(markdown, encoding="utf-8")
                row.update(
                    ok=True,
                    output=str(destination.relative_to(output)),
                    output_bytes=len(markdown.encode()),
                    output_sha256=sha256(destination),
                    deterministic=len(set(output_hashes)) == 1,
                    iteration_output_sha256=output_hashes,
                    timings_ms=[round(value, 3) for value in timings],
                    min_ms=round(min(timings), 3),
                    median_ms=round(statistics.median(timings), 3),
                )
                print(f"{tool:10} {source.name:42} {row['median_ms']:10.2f} ms")
            except Exception as error:
                row.update(ok=False, error=str(error), timings_ms=[])
                print(f"{tool:10} {source.name:42} ERROR {error}", file=sys.stderr)
            rows.append(row)
            (output / "results.json").write_text(json.dumps(rows, indent=2) + "\n")

    report(rows, output, args.suite)
    failures = sum(not row["ok"] for row in rows)
    print(f"\nWrote {output}; {failures} conversion failure(s)")
    return int(failures > 0)


if __name__ == "__main__":
    raise SystemExit(main())
