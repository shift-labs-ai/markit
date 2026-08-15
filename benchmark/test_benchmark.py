import hashlib
import json
import tempfile
import unittest
from pathlib import Path

import stat

from bench import (
    canonical_output,
    safe_name,
    sha256,
    verify_shitty_pdf_bench_corpus,
    version,
)
from generate_shitty_pdf_bench_anchors import word_tokens
from score_shitty_pdf_bench import evaluate, normalized, normalized_words
from score_office import containment, counts, trigrams
from setup_shitty_pdf_bench import download


class BenchmarkHelpersTest(unittest.TestCase):
    def test_assertion_types(self):
        markdown = "# AURIX™ TC3xx\n\n| Field | Bits |\n| --- | --- |\n| CAT | 8 |"
        self.assertTrue(evaluate(markdown, {"type": "contains", "value": "AURIX TC3xx"}))
        self.assertTrue(
            evaluate(markdown, {"type": "contains_words", "value": "Field Bits"})
        )
        self.assertTrue(evaluate(markdown, {"type": "regex", "value": r"CAT\s*\|\s*8"}))
        self.assertTrue(evaluate(markdown, {"type": "min_chars", "value": 20}))
        self.assertTrue(evaluate(markdown, {"type": "min_table_rows", "value": 2}))
        self.assertTrue(evaluate(markdown, {"type": "not_contains", "value": "garbage"}))

    def test_normalization_ignores_marks_case_and_spacing(self):
        self.assertEqual(normalized("Intel®   PCH"), "intel pch")

    def test_version_prefers_manifest_over_self_reported_string(self):
        # liteparse 2.12.0 ships a hardcoded `.version("2.0.0")`, so trusting
        # the CLI records a version two majors adrift from what actually ran.
        with tempfile.TemporaryDirectory() as temp:
            package = Path(temp) / "node_modules" / "@vendor" / "tool"
            (package / "dist").mkdir(parents=True)
            package.joinpath("package.json").write_text(
                json.dumps({"name": "@vendor/tool", "version": "2.12.0"})
            )
            cli = package / "dist" / "cli.js"
            cli.write_text("#!/bin/sh\necho 2.0.0\n")
            cli.chmod(cli.stat().st_mode | stat.S_IXUSR)
            self.assertEqual(version(str(cli)), "@vendor/tool@2.12.0")

            # A binary outside any package still falls back to the CLI probe.
            loose = Path(temp) / "loose.sh"
            loose.write_text("#!/bin/sh\necho 7.7.7\n")
            loose.chmod(loose.stat().st_mode | stat.S_IXUSR)
            self.assertEqual(version(str(loose)), "7.7.7")

    def test_structure_counts_and_containment(self):
        markdown = "# Title\n\n- one\n- two\n\n| A | B |\n| --- | --- |\n| 1 | 2 |"
        result = counts(markdown)
        self.assertEqual(result["headings"], 1)
        self.assertEqual(result["list_items"], 2)
        self.assertEqual(result["table_rows"], 2)
        grams = trigrams("one two three four")
        self.assertEqual(containment(grams, grams), 1.0)

    def test_temporary_image_paths_do_not_change_semantic_output(self):
        first = "![x](/private/tmp/markit-images-123/p1-img0.png)"
        second = "![x](/private/tmp/markit-images-987/p1-img0.png)"
        self.assertNotEqual(first, second)
        self.assertEqual(canonical_output(first), canonical_output(second))

    def test_hash_and_output_name_are_stable(self):
        with tempfile.TemporaryDirectory() as temp:
            path = Path(temp) / "sample.pdf"
            path.write_bytes(b"abc")
            self.assertEqual(
                sha256(path),
                "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
            )
            self.assertEqual(safe_name(path), "sample.pdf.md")

    def test_anchor_word_tokenization_is_markup_neutral(self):
        self.assertEqual(
            word_tokens("Let `targetIndex` be max(len, 0)."),
            ["let", "targetindex", "be", "max", "len", "0"],
        )
        self.assertEqual(
            normalized_words(r"$V_{OL } = 0.3 \times$ VCC"),
            "vol 0 3 vcc",
        )
        self.assertEqual(
            normalized_words(r"expand $\langle value\rangle$ using \edef"),
            "expand hvaluei using edef",
        )
        self.assertEqual(
            normalized_words("relativeTarget &lt; 0; ar-<br>rested"),
            "relativetarget 0 arrested",
        )

    def test_shitty_pdf_bench_manifest_is_complete(self):
        manifest_path = Path(__file__).with_name("shitty-pdf-bench.json")
        documents = json.loads(manifest_path.read_text())["documents"]
        self.assertEqual(len(documents), 40)
        self.assertEqual(len({item["file"] for item in documents}), 40)
        self.assertEqual(len({item["sha256"] for item in documents}), 40)
        for item in documents:
            self.assertTrue(item["url"].startswith("https://"))
            self.assertEqual(len(item["sha256"]), 64)
            self.assertGreater(item["bytes"], 0)
            self.assertGreater(item["pages"], 0)
            self.assertTrue(item["categories"])

        anchors = json.loads(
            Path(__file__).with_name("shitty-pdf-bench-anchors.json").read_text()
        )
        self.assertEqual(anchors["manifest_sha256"], sha256(manifest_path))
        self.assertEqual(set(anchors["documents"]), {item["file"] for item in documents})
        self.assertEqual(
            sum(len(item["checks"]) for item in anchors["documents"].values()),
            155,
        )

    def test_shitty_pdf_bench_manifest_hash_mismatch_fails(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            source = root / "fixture.pdf"
            source.write_bytes(b"%PDF-wrong")
            manifest = root / "manifest.json"
            manifest.write_text(
                json.dumps(
                    {
                        "documents": [
                            {
                                "file": source.name,
                                "sha256": hashlib.sha256(b"%PDF-right").hexdigest(),
                            }
                        ]
                    }
                )
            )
            with self.assertRaisesRegex(RuntimeError, "SHA-256 mismatch"):
                verify_shitty_pdf_bench_corpus([source], manifest)

    def test_download_is_atomic_and_cacheable(self):
        payload = b"%PDF-1.7\nminimal fixture"
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            source = root / "source.pdf"
            output = root / "output"
            output.mkdir()
            source.write_bytes(payload)
            document = {
                "file": "fixture.pdf",
                "url": source.as_uri(),
                "bytes": len(payload),
                "sha256": hashlib.sha256(payload).hexdigest(),
            }
            self.assertEqual(download(document, output), ("fixture.pdf", "downloaded"))
            self.assertEqual(download(document, output), ("fixture.pdf", "cached"))
            self.assertEqual((output / "fixture.pdf").read_bytes(), payload)
            self.assertFalse((output / "fixture.pdf.part").exists())


if __name__ == "__main__":
    unittest.main()
