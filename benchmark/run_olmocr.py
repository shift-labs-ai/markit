#!/usr/bin/env python3
"""Convert olmOCR-bench PDFs with all three tools and run its official scorer."""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
import time
import shutil
from pathlib import Path

from bench import ConversionError, TOOLS, run_once, tool_bins, version, write_json


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("root", type=Path, help="directory created by setup-olmocr.sh")
    parser.add_argument("--tools", default=",".join(TOOLS))
    parser.add_argument("--resume", action="store_true")
    parser.add_argument("--timeout", type=float, default=300)
    args = parser.parse_args()

    bench_data = args.root / "data" / "bench_data"
    pdf_root = bench_data / "pdfs"
    olmocr = args.root / "olmocr"
    if not pdf_root.is_dir() or not olmocr.is_dir():
        parser.error("invalid root; run benchmark/setup-olmocr.sh first")

    tools = [tool.strip() for tool in args.tools.split(",") if tool.strip()]
    unknown = set(tools) - set(TOOLS)
    if unknown or not tools:
        parser.error(f"invalid --tools selection: {sorted(unknown)}")
    if args.timeout <= 0:
        parser.error("--timeout must be > 0")
    bins = tool_bins(tools)
    pdfs = sorted(pdf_root.rglob("*.pdf"))
    if not pdfs:
        parser.error("olmOCR dataset contains no PDFs")
    result_dir = Path(__file__).with_name("results") / "olmocr"
    result_dir.mkdir(parents=True, exist_ok=True)
    timings_path = result_dir / "timings.json"
    previous = (
        json.loads(timings_path.read_text())
        if args.resume and timings_path.exists()
        else []
    )
    timings_by_key = {(row["tool"], row["file"]): row for row in previous}
    pinned = (args.root / "PINNED").read_text().splitlines() if (args.root / "PINNED").exists() else []
    provenance = {
        "timestamp": time.time(),
        "pinned": pinned,
        "timeout_seconds": args.timeout,
        "tools": {
            tool: {"binary": bins[tool], "version": version(bins[tool])}
            for tool in tools
        },
    }
    write_json(result_dir / "provenance.json", provenance)
    failures = 0
    for tool in tools:
        candidate = bench_data / tool
        if not args.resume:
            shutil.rmtree(candidate, ignore_errors=True)
        started = time.perf_counter()
        converted = 0
        for index, pdf in enumerate(pdfs, 1):
            relative = pdf.relative_to(pdf_root)
            destination = candidate / relative.parent / f"{pdf.stem}_pg1_repeat1.md"
            if destination.exists() and args.resume:
                continue
            destination.parent.mkdir(parents=True, exist_ok=True)
            try:
                markdown, elapsed_ms = run_once(
                    tool, bins[tool], pdf, args.timeout
                )
                destination.write_text(markdown, encoding="utf-8")
                timings_by_key[(tool, str(relative))] = {
                    "tool": tool,
                    "file": str(relative),
                    "ok": True,
                    "ms": round(elapsed_ms, 3),
                }
                converted += 1
            except Exception as error:
                # The official scorer requires one candidate file per input.
                # Preserve conversion failures as empty Markdown so they score
                # zero rather than making the entire candidate unevaluable.
                destination.write_text("", encoding="utf-8")
                timings_by_key[(tool, str(relative))] = {
                    "tool": tool,
                    "file": str(relative),
                    "ok": False,
                    "error": str(error),
                    "failure_ms": round(error.elapsed_ms, 3)
                    if isinstance(error, ConversionError)
                    else None,
                }
                failures += 1
                print(f"{tool} {relative}: ERROR {error}", file=sys.stderr)
            if index % 100 == 0:
                print(f"{tool}: {index}/{len(pdfs)}")
        print(f"{tool}: converted {converted} files in {time.perf_counter() - started:.2f}s")

    timings = sorted(
        timings_by_key.values(), key=lambda row: (row["tool"], row["file"])
    )
    write_json(timings_path, timings)

    env = dict(os.environ)
    env["PYTHONPATH"] = str(olmocr) + os.pathsep + env.get("PYTHONPATH", "")
    scorer_python = os.environ.get("OLMOCR_PYTHON", sys.executable)
    for tool in tools:
        output = result_dir / f"{tool}.txt"
        with output.open("w") as stream:
            result = subprocess.run(
                [
                    scorer_python,
                    "-m",
                    "olmocr.bench.benchmark",
                    "--dir",
                    str(bench_data),
                    "--candidate",
                    tool,
                    "--force",
                ],
                cwd=olmocr,
                env=env,
                stdout=stream,
                stderr=subprocess.STDOUT,
            )
        if result.returncode:
            failures += 1
            print(f"official scorer failed for {tool}; see {output}", file=sys.stderr)
        else:
            print(f"official score written to {output}")
    if all((result_dir / f"{tool}.txt").exists() for tool in TOOLS):
        subprocess.run(
            [sys.executable, str(Path(__file__).with_name("report_olmocr.py")), str(result_dir)],
            check=True,
        )
    return int(failures > 0)


if __name__ == "__main__":
    raise SystemExit(main())
