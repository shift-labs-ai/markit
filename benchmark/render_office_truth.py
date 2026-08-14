#!/usr/bin/env python3
"""Render office documents to page images for blind quality judging."""

import argparse
import re
import shutil
import subprocess
import tempfile
import zipfile
from pathlib import Path

RENDERABLE = {".docx", ".pptx", ".xlsx"}


def render_epub_truth(source: Path, destination: Path) -> None:
    parts = []
    with zipfile.ZipFile(source) as archive:
        names = sorted(
            name
            for name in archive.namelist()
            if name.lower().endswith((".xhtml", ".html", ".htm"))
        )
        for name in names:
            html = archive.read(name).decode("utf-8", "replace")
            html = re.sub(r"(?s)<(script|style)[^>]*>.*?</\\1>", " ", html)
            text = re.sub(r"\\s+", " ", re.sub(r"<[^>]+>", " ", html)).strip()
            if text:
                parts.append(text)
    destination.mkdir(parents=True, exist_ok=True)
    (destination / "truth.txt").write_text(" ".join(parts)[:30_000], encoding="utf-8")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--corpus", type=Path, default=Path(__file__).with_name("corpus"))
    parser.add_argument("--output", type=Path, default=Path(__file__).with_name("results") / "office" / "truth")
    parser.add_argument("--max-pages", type=int, default=6)
    parser.add_argument("--dpi", type=int, default=100)
    args = parser.parse_args()

    soffice = shutil.which("soffice") or shutil.which("libreoffice")
    pdftoppm = shutil.which("pdftoppm")
    if not soffice or not pdftoppm:
        raise SystemExit("soffice/libreoffice and pdftoppm are required")

    args.output.mkdir(parents=True, exist_ok=True)
    for source in sorted(args.corpus.iterdir()):
        destination = args.output / source.name
        if source.suffix.lower() == ".epub":
            if not (destination / "truth.txt").exists():
                render_epub_truth(source, destination)
                print(f"{source.name}: EPUB text truth")
            continue
        if source.suffix.lower() not in RENDERABLE:
            continue
        if (destination / "page-01.png").exists():
            continue
        with tempfile.TemporaryDirectory(prefix="markit-truth-") as temp:
            result = subprocess.run(
                [soffice, "--headless", "--convert-to", "pdf", "--outdir", temp, str(source)],
                capture_output=True,
                text=True,
            )
            pdf = Path(temp) / f"{source.stem}.pdf"
            if result.returncode or not pdf.exists():
                print(f"SKIP {source.name}: {result.stderr.strip()[:300]}")
                continue
            destination.mkdir(parents=True, exist_ok=True)
            subprocess.run(
                [pdftoppm, "-png", "-r", str(args.dpi), "-f", "1", "-l", str(args.max_pages), str(pdf), str(destination / "page")],
                check=True,
                capture_output=True,
            )
            for index, page in enumerate(sorted(destination.glob("page-*.png")), 1):
                page.rename(destination / f"page-{index:02}.png")
            print(f"{source.name}: {len(list(destination.glob('page-*.png')))} page(s)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
