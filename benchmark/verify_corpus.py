#!/usr/bin/env python3
"""Verify a benchmark corpus against a committed SHA-256 manifest."""

import argparse
import hashlib
import json
from pathlib import Path


def digest(path: Path) -> str:
    value = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            value.update(chunk)
    return value.hexdigest()


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("manifest", type=Path)
    parser.add_argument("corpus", type=Path)
    args = parser.parse_args()
    documents = json.loads(args.manifest.read_text())["documents"]
    failed = False
    for document in documents:
        path = args.corpus / document["file"]
        actual = digest(path) if path.exists() else "missing"
        ok = actual == document["sha256"]
        print(f"{'OK' if ok else 'FAIL'} {document['file']}")
        failed |= not ok
    return int(failed)


if __name__ == "__main__":
    raise SystemExit(main())
