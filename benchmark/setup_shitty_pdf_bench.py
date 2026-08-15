#!/usr/bin/env python3
"""Download and verify the public shitty-pdf-bench corpus."""

import argparse
import concurrent.futures
import hashlib
import json
import os
import sys
import urllib.request
from pathlib import Path

CHUNK_SIZE = 1024 * 1024
USER_AGENT = "markit-benchmark/0.6 (+https://github.com/shift-labs-ai/markit)"


def digest(path: Path) -> str:
    value = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(CHUNK_SIZE), b""):
            value.update(chunk)
    return value.hexdigest()


def download(document: dict[str, object], output_dir: Path) -> tuple[str, str]:
    name = str(document["file"])
    expected_hash = str(document["sha256"])
    expected_size = int(document["bytes"])
    destination = output_dir / name
    temporary = destination.with_suffix(destination.suffix + ".part")

    if (
        destination.is_file()
        and destination.stat().st_size == expected_size
        and digest(destination) == expected_hash
    ):
        return name, "cached"

    temporary.unlink(missing_ok=True)
    value = hashlib.sha256()
    size = 0
    request = urllib.request.Request(str(document["url"]), headers={"User-Agent": USER_AGENT})
    try:
        with urllib.request.urlopen(request, timeout=60) as response, temporary.open("wb") as stream:
            while chunk := response.read(CHUNK_SIZE):
                stream.write(chunk)
                value.update(chunk)
                size += len(chunk)
        if size != expected_size:
            raise ValueError(f"expected {expected_size} bytes, received {size}")
        actual_hash = value.hexdigest()
        if actual_hash != expected_hash:
            raise ValueError(f"expected SHA-256 {expected_hash}, received {actual_hash}")
        with temporary.open("rb") as stream:
            if stream.read(5) != b"%PDF-":
                raise ValueError("download does not start with a PDF header")
        temporary.replace(destination)
        return name, "downloaded"
    except Exception:
        temporary.unlink(missing_ok=True)
        raise


def main() -> int:
    script_dir = Path(__file__).resolve().parent
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--manifest", type=Path, default=script_dir / "shitty-pdf-bench.json"
    )
    parser.add_argument(
        "--output",
        type=Path,
        default=Path(
            os.environ.get("SHITTY_PDF_BENCH_DIR", script_dir / "corpus-shitty-pdf-bench")
        ),
    )
    parser.add_argument("--category", action="append", default=[])
    parser.add_argument("--file", action="append", default=[])
    parser.add_argument("--workers", type=int, default=4)
    args = parser.parse_args()

    manifest = json.loads(args.manifest.read_text())
    documents = manifest["documents"]
    if args.category:
        requested = set(args.category)
        documents = [item for item in documents if requested.intersection(item["categories"])]
    if args.file:
        requested_files = set(args.file)
        documents = [item for item in documents if item["file"] in requested_files]
        missing = requested_files.difference(item["file"] for item in documents)
        if missing:
            parser.error(f"unknown files: {', '.join(sorted(missing))}")
    if not documents:
        parser.error("selection contains no documents")
    if args.workers < 1:
        parser.error("--workers must be at least 1")

    args.output.mkdir(parents=True, exist_ok=True)
    failures: list[tuple[str, Exception]] = []
    with concurrent.futures.ThreadPoolExecutor(max_workers=args.workers) as executor:
        futures = {executor.submit(download, item, args.output): item for item in documents}
        for future in concurrent.futures.as_completed(futures):
            name = str(futures[future]["file"])
            try:
                _, status = future.result()
                print(f"{status.upper():10} {name}")
            except Exception as error:
                failures.append((name, error))
                print(f"FAILED     {name}: {error}", file=sys.stderr)

    if failures:
        print(f"\n{len(failures)} of {len(documents)} downloads failed", file=sys.stderr)
        return 1
    total_bytes = sum(int(item["bytes"]) for item in documents)
    total_pages = sum(int(item["pages"]) for item in documents)
    print(f"\nVerified {len(documents)} PDFs, {total_pages:,} pages, {total_bytes / 1_000_000:.1f} MB")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
