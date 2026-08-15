#!/usr/bin/env python3
"""Generate blind fixed-page content anchors from the shitty-pdf-bench corpus.

The generator reads only source PDFs and their committed manifest. It never reads
converter output. Five deterministic page quantiles are sampled per document;
repeated chrome, title-like lines, and RTL lines whose Poppler order is ambiguous
are excluded before selecting one seven-word anchor per sampled page.
"""

from __future__ import annotations

import argparse
import concurrent.futures
import hashlib
import json
import re
import subprocess
from collections import Counter, defaultdict
from pathlib import Path

PAGE_FRACTIONS = (0.05, 0.25, 0.50, 0.75, 0.95)
WORD_PATTERN = re.compile(r"[^\W_]+(?:['’][^\W_]+)?|\d+(?:[.,]\d+)*", re.UNICODE)


def word_tokens(text: str) -> list[str]:
    return WORD_PATTERN.findall(text.casefold())


def source_digest(path: Path) -> str:
    value = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            value.update(chunk)
    return value.hexdigest()


def candidate_lines(document: dict, page: int, corpus: Path) -> tuple[str, int, list[tuple]]:
    source = corpus / document["file"]
    result = subprocess.run(
        ["pdftotext", "-f", str(page), "-l", str(page), "-nopgbrk", str(source), "-"],
        capture_output=True,
        timeout=60,
    )
    lines = result.stdout.decode("utf-8", "replace").splitlines()
    midpoint = len(lines) / 2
    title_words = set(word_tokens(document["title"]))
    candidates = []
    for line_index, line in enumerate(lines):
        # Poppler's non-layout mode can splice adjacent columns into a token
        # (for example, "res-" + "In-" becoming "resIn-"). Reject those
        # mechanically detectable cross-column joins before choosing anchors.
        if re.search(r"[a-z][A-Z][A-Za-z]*-|\w-\s+\w", line):
            continue
        words = word_tokens(line)
        alphabetic_words = sum(any(character.isalpha() for character in word) for word in words)
        letter_count = sum(character.isalpha() for character in line)
        rtl_count = sum("\u0590" <= character <= "\u08ff" for character in line)
        rtl_ratio = rtl_count / letter_count if letter_count else 0
        if not (7 <= len(words) <= 30 and alphabetic_words >= 5 and rtl_ratio < 0.3):
            continue
        start = max(0, (len(words) - 7) // 2)
        phrase = " ".join(words[start : start + 7])
        title_overlap = len(set(words) & title_words) / max(1, len(title_words))
        candidates.append(
            (title_overlap, abs(len(words) - 12), abs(line_index - midpoint), phrase)
        )
    return document["file"], page, sorted(candidates)


def generate(manifest: dict, corpus: Path, workers: int) -> dict:
    jobs = []
    for document in manifest["documents"]:
        pages = {
            max(1, min(document["pages"], round(document["pages"] * fraction)))
            for fraction in PAGE_FRACTIONS
        }
        jobs.extend((document, page, corpus) for page in sorted(pages))

    raw_candidates: dict[str, list[tuple[int, list[tuple]]]] = defaultdict(list)
    with concurrent.futures.ThreadPoolExecutor(max_workers=workers) as executor:
        for filename, page, candidates in executor.map(lambda args: candidate_lines(*args), jobs):
            raw_candidates[filename].append((page, candidates))

    documents = {}
    for document in manifest["documents"]:
        filename = document["file"]
        pages = raw_candidates[filename]
        frequencies = Counter(
            candidate[3] for _, candidates in pages for candidate in candidates
        )
        checks = []
        for page, candidates in pages:
            unique_non_title = [
                candidate
                for candidate in candidates
                if frequencies[candidate[3]] == 1 and candidate[0] < 0.6
            ]
            unique = [
                candidate for candidate in candidates if frequencies[candidate[3]] == 1
            ]
            eligible = unique_non_title or unique or candidates
            if eligible:
                checks.append(
                    {"type": "contains_words", "page": page, "value": eligible[0][3]}
                )
        documents[filename] = {
            "sha256": document["sha256"],
            "checks": checks,
        }

    return {
        "schema": 1,
        "description": "Blind fixed-page content anchors generated only from source PDFs.",
        "method": {
            "page_fractions": list(PAGE_FRACTIONS),
            "words_per_anchor": 7,
            "source_extractor": "pdftotext",
            "excludes": ["repeated chrome", "title-like lines", "RTL lines with ambiguous extractor order"],
        },
        "manifest_sha256": source_digest(Path(manifest["_path"])),
        "documents": documents,
    }


def main() -> int:
    script_dir = Path(__file__).resolve().parent
    parser = argparse.ArgumentParser()
    parser.add_argument("corpus", type=Path)
    parser.add_argument(
        "--manifest", type=Path, default=script_dir / "shitty-pdf-bench.json"
    )
    parser.add_argument(
        "--output", type=Path, default=script_dir / "shitty-pdf-bench-anchors.json"
    )
    parser.add_argument("--workers", type=int, default=8)
    args = parser.parse_args()
    if args.workers < 1:
        parser.error("--workers must be at least 1")

    manifest = json.loads(args.manifest.read_text())
    manifest["_path"] = str(args.manifest)
    result = generate(manifest, args.corpus, args.workers)
    args.output.write_text(json.dumps(result, indent=2, ensure_ascii=False) + "\n")
    count = sum(len(document["checks"]) for document in result["documents"].values())
    print(f"Wrote {count} blind anchors to {args.output}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
