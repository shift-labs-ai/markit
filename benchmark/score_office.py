#!/usr/bin/env python3
"""Deterministic structure and pairwise content metrics for office outputs."""

import argparse
import json
import re
import statistics
from itertools import combinations
from pathlib import Path

TOOLS = ("markit", "anydoc", "liteparse")


def trigrams(text: str) -> set[tuple[str, str, str]]:
    words = re.findall(r"\w+", text.casefold())
    return set(zip(words, words[1:], words[2:]))


def containment(reference: set, candidate: set) -> float:
    return len(reference & candidate) / len(reference) if reference else 1.0


def counts(text: str) -> dict:
    table_rows = [line for line in text.splitlines() if re.match(r"^\s*\|.*\|\s*$", line)]
    separators = [line for line in table_rows if re.match(r"^\s*\|[\s:|-]+\|\s*$", line)]
    return {
        "chars": len(text),
        "headings": len(re.findall(r"^#{1,6}\s", text, re.MULTILINE)),
        "table_rows": len(table_rows) - len(separators),
        "list_items": len(re.findall(r"^\s*(?:[-*+]|\d+[.)])\s", text, re.MULTILINE)),
        "links": len(re.findall(r"\[[^]]*\]\([^)]+\)", text)),
        "footnotes": len(re.findall(r"^\[\^[^]]+\]:", text, re.MULTILINE)),
    }


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("results", type=Path)
    args = parser.parse_args()
    documents = {}
    for tool in TOOLS:
        directory = args.results / tool
        for output in sorted(directory.glob("*.md")) if directory.exists() else []:
            documents.setdefault(output.name, {})[tool] = output.read_text(errors="replace")

    details = []
    for filename, outputs in documents.items():
        row = {"file": filename, "tools": {tool: counts(text) for tool, text in outputs.items()}, "pairs": {}}
        for left, right in combinations(sorted(outputs), 2):
            left_grams, right_grams = trigrams(outputs[left]), trigrams(outputs[right])
            row["pairs"][f"{left}:{right}"] = {
                f"{left}_found_in_{right}": round(containment(left_grams, right_grams), 4),
                f"{right}_found_in_{left}": round(containment(right_grams, left_grams), 4),
            }
        details.append(row)

    pair_values = {}
    for left, right in combinations(TOOLS, 2):
        forward, reverse = [], []
        for document in details:
            pair = document["pairs"].get(f"{left}:{right}") or document["pairs"].get(f"{right}:{left}")
            if not pair:
                continue
            forward.append(pair.get(f"{left}_found_in_{right}", pair.get(f"{left}_found_in_{right}", 0)))
            reverse.append(pair.get(f"{right}_found_in_{left}", pair.get(f"{right}_found_in_{left}", 0)))
        pair_values[f"{left}:{right}"] = {
            "documents": len(forward),
            f"{left}_found_in_{right}": round(statistics.fmean(forward), 4) if forward else None,
            f"{right}_found_in_{left}": round(statistics.fmean(reverse), 4) if reverse else None,
        }

    result = {"schema": 1, "pairwise_trigram_containment": pair_values, "documents": details}
    destination = args.results / "deterministic-quality.json"
    destination.write_text(json.dumps(result, indent=2) + "\n")
    print(f"wrote {destination}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
