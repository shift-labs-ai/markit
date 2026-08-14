#!/usr/bin/env python3
"""Convert olmOCR-bench PDFs with all three tools and run its official scorer."""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
import time
from pathlib import Path

from bench import tool_bins, run_once, TOOLS


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("root", type=Path, help="directory created by setup-olmocr.sh")
    parser.add_argument("--tools", default=",".join(TOOLS))
    parser.add_argument("--force", action="store_true")
    args = parser.parse_args()

    bench_data = args.root / "data" / "bench_data"
    pdf_root = bench_data / "pdfs"
    olmocr = args.root / "olmocr"
    if not pdf_root.is_dir() or not olmocr.is_dir():
        parser.error("invalid root; run benchmark/setup-olmocr.sh first")

    tools = [tool.strip() for tool in args.tools.split(",") if tool.strip()]
    bins = tool_bins()
    pdfs = sorted(pdf_root.rglob("*.pdf"))
    result_dir = Path(__file__).with_name("results") / "olmocr"
    result_dir.mkdir(parents=True, exist_ok=True)
    timings_path = result_dir / "timings.json"
    previous = json.loads(timings_path.read_text()) if timings_path.exists() else []
    timings_by_key = {(row["tool"], row["file"]): row for row in previous}
    failures = 0
    for tool in tools:
        candidate = bench_data / tool
        started = time.perf_counter()
        converted = 0
        for index, pdf in enumerate(pdfs, 1):
            relative = pdf.relative_to(pdf_root)
            destination = candidate / relative.parent / f"{pdf.stem}_pg1_repeat1.md"
            if destination.exists() and not args.force:
                continue
            destination.parent.mkdir(parents=True, exist_ok=True)
            try:
                markdown, elapsed_ms = run_once(tool, bins[tool], pdf)
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
                }
                failures += 1
                print(f"{tool} {relative}: ERROR {error}", file=sys.stderr)
            if index % 100 == 0:
                print(f"{tool}: {index}/{len(pdfs)}")
        print(f"{tool}: converted {converted} files in {time.perf_counter() - started:.2f}s")

    timings = sorted(timings_by_key.values(), key=lambda row: (row["tool"], row["file"]))
    timings_path.write_text(json.dumps(timings, indent=2) + "\n")

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
