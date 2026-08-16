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
import re
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


class ConversionError(RuntimeError):
    def __init__(self, message: str, elapsed_ms: float):
        super().__init__(message)
        self.elapsed_ms = elapsed_ms


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


def tool_bins(tools: list[str] | tuple[str, ...] = TOOLS) -> dict[str, str]:
    candidates = {
        "markit": (
            "MARKIT_BIN",
            [ROOT / "rust" / "target" / "release" / "markit", "markit"],
        ),
        "anydoc": ("ANYDOC_BIN", ["anydoc"]),
        "liteparse": ("LITEPARSE_BIN", ["lit"]),
    }
    return {
        tool: executable(candidates[tool][0], candidates[tool][1]) for tool in tools
    }


def package_version(binary: str) -> str | None:
    """Version from the owning package.json for an npm-installed binary.

    A CLI's own `--version` string is only as trustworthy as its author:
    liteparse 2.12.0 ships a hardcoded `.version("2.0.0")`, so probing the
    binary records a version two majors adrift from what actually ran.
    The installed manifest is authoritative.
    """
    path = Path(binary).resolve()
    for parent in path.parents:
        manifest = parent / "package.json"
        if not manifest.is_file():
            continue
        try:
            data = json.loads(manifest.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError):
            return None
        name, installed = data.get("name"), data.get("version")
        if name and installed:
            return f"{name}@{installed}"
        return None
    return None


def version(binary: str) -> str:
    resolved = package_version(binary)
    if resolved:
        return resolved
    for flag in ("--version", "-V"):
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


def run_once(
    tool: str, binary: str, source: Path, timeout_seconds: float = 300
) -> tuple[str, float]:
    with tempfile.TemporaryDirectory(prefix="markit-bench-") as temp:
        output = Path(temp) / "output.md"
        # Every tool runs without image extraction. anydoc writes no image
        # files and liteparse is given --image-mode off, so leaving markit
        # on its default (which decodes and re-encodes every embedded
        # image to a temp directory) measured it doing strictly more work
        # than the tools it is compared against.
        if tool == "markit":
            command = [binary, str(source), "--quiet", "--no-images"]
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
        result = subprocess.run(
            command, capture_output=True, timeout=timeout_seconds
        )
        elapsed_ms = (time.perf_counter_ns() - started) / 1_000_000
        if result.returncode != 0:
            error = result.stderr.decode("utf-8", "replace").strip()
            raise ConversionError(
                error[:1000] or f"exit {result.returncode}", elapsed_ms
            )
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
    elif suite == "shitty-pdf-bench":
        files = [path for path in files if path.suffix.lower() == ".pdf"]
    if not files:
        raise RuntimeError(f"no {suite} files found in {corpus}")
    return files


def safe_name(path: Path) -> str:
    return path.name.replace("/", "_") + ".md"


def canonical_output(markdown: str) -> str:
    """Remove run-specific temporary image-directory IDs for semantic hashing."""
    return re.sub(r"(?<=/markit-images-)\d+(?=/)", "<run>", markdown)


def write_json(path: Path, value: object) -> None:
    temporary = path.with_suffix(path.suffix + ".tmp")
    temporary.write_text(json.dumps(value, indent=2) + "\n", encoding="utf-8")
    temporary.replace(path)


def verify_shitty_pdf_bench_corpus(files: list[Path], manifest: Path) -> None:
    raw_documents = json.loads(manifest.read_text(encoding="utf-8"))["documents"]
    documents = (
        {item["file"]: item for item in raw_documents}
        if isinstance(raw_documents, list)
        else raw_documents
    )
    actual_names = {path.name for path in files}
    expected_names = set(documents)
    if actual_names != expected_names:
        missing = sorted(expected_names - actual_names)
        extra = sorted(actual_names - expected_names)
        raise RuntimeError(
            f"shitty-pdf-bench corpus mismatch; missing={missing}, extra={extra}"
        )
    for source in files:
        expected = documents[source.name]["sha256"]
        actual = sha256(source)
        if actual != expected:
            raise RuntimeError(
                f"SHA-256 mismatch for {source.name}: expected {expected}, got {actual}"
            )


def report(rows: list[dict], output: Path, suite: str, tools: list[str]) -> None:
    lines = [
        f"# {suite} benchmark: {' vs '.join(tools)}",
        "",
        "End-to-end warm CLI wall time. PDF runs disable Liteparse OCR.",
        "",
        "| Tool | Passed | Failed | Median ms/doc | Total seconds |",
        "| --- | ---: | ---: | ---: | ---: |",
    ]
    for tool in tools:
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
    parser.add_argument("suite", choices=("office", "shitty-pdf-bench"))
    parser.add_argument("--corpus", type=Path)
    parser.add_argument("--output", type=Path)
    parser.add_argument("--tools", default=",".join(TOOLS))
    parser.add_argument("--warmups", type=int, default=1)
    parser.add_argument("--iterations", type=int, default=3)
    parser.add_argument("--timeout", type=float, default=300)
    parser.add_argument(
        "--manifest",
        type=Path,
        default=BENCH / "shitty-pdf-bench.json",
        help="shitty-pdf-bench corpus manifest",
    )
    args = parser.parse_args()

    if args.corpus:
        corpus = args.corpus
    elif args.suite == "office":
        corpus = BENCH / "corpus"
    elif os.environ.get("SHITTY_PDF_BENCH_DIR"):
        corpus = Path(os.environ["SHITTY_PDF_BENCH_DIR"])
    else:
        parser.error("--corpus or SHITTY_PDF_BENCH_DIR is required for shitty-pdf-bench")
    output = args.output or BENCH / "results" / args.suite
    output.mkdir(parents=True, exist_ok=True)

    selected_tools = [tool.strip() for tool in args.tools.split(",") if tool.strip()]
    unknown = set(selected_tools) - set(TOOLS)
    if unknown:
        parser.error(f"unknown tools: {sorted(unknown)}")
    if not selected_tools:
        parser.error("--tools must select at least one tool")
    if args.warmups < 0 or args.iterations < 1 or args.timeout <= 0:
        parser.error("warmups must be >= 0; iterations and timeout must be > 0")

    files = corpus_files(args.suite, corpus)
    if args.suite == "shitty-pdf-bench":
        verify_shitty_pdf_bench_corpus(files, args.manifest)

    bins = tool_bins(selected_tools)
    for tool in selected_tools:
        shutil.rmtree(output / tool, ignore_errors=True)

    git_commit = subprocess.run(
        ["git", "rev-parse", "HEAD"],
        cwd=ROOT,
        capture_output=True,
        text=True,
    )
    provenance = {
        "suite": args.suite,
        "timestamp": time.time(),
        "platform": platform.platform(),
        "machine": platform.machine(),
        "python": platform.python_version(),
        "warmups": args.warmups,
        "iterations": args.iterations,
        "timeout_seconds": args.timeout,
        "git_commit": git_commit.stdout.strip() if git_commit.returncode == 0 else None,
        "corpus": [
            {"file": source.name, "bytes": source.stat().st_size, "sha256": sha256(source)}
            for source in files
        ],
        "tools": {
            tool: {"binary": bins[tool], "version": version(bins[tool])}
            for tool in selected_tools
        },
    }
    write_json(output / "provenance.json", provenance)

    rows: list[dict] = []
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
                    run_once(tool, bins[tool], source, args.timeout)
                timings = []
                markdown = ""
                output_hashes = []
                raw_output_hashes = []
                for _ in range(args.iterations):
                    markdown, elapsed = run_once(
                        tool, bins[tool], source, args.timeout
                    )
                    timings.append(elapsed)
                    raw_output_hashes.append(hashlib.sha256(markdown.encode()).hexdigest())
                    output_hashes.append(
                        hashlib.sha256(canonical_output(markdown).encode()).hexdigest()
                    )
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
                    raw_iteration_output_sha256=raw_output_hashes,
                    timings_ms=[round(value, 3) for value in timings],
                    min_ms=round(min(timings), 3),
                    median_ms=round(statistics.median(timings), 3),
                )
                print(f"{tool:10} {source.name:42} {row['median_ms']:10.2f} ms")
            except Exception as error:
                row.update(
                    ok=False,
                    error=str(error),
                    timings_ms=[],
                    failure_ms=round(error.elapsed_ms, 3)
                    if isinstance(error, ConversionError)
                    else None,
                )
                print(f"{tool:10} {source.name:42} ERROR {error}", file=sys.stderr)
            rows.append(row)
            write_json(output / "results.json", rows)

    report(rows, output, args.suite, selected_tools)
    failures = sum(not row["ok"] for row in rows)
    print(f"\nWrote {output}; {failures} conversion failure(s)")
    return int(failures > 0)


if __name__ == "__main__":
    raise SystemExit(main())
