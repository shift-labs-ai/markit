import tempfile
import unittest
from pathlib import Path

from bench import safe_name, sha256
from score_horrible import evaluate, normalized
from score_office import containment, counts, trigrams


class BenchmarkHelpersTest(unittest.TestCase):
    def test_horrible_assertion_types(self):
        markdown = "# AURIX™ TC3xx\n\n| Field | Bits |\n| --- | --- |\n| CAT | 8 |"
        self.assertTrue(evaluate(markdown, {"type": "contains", "value": "AURIX TC3xx"}))
        self.assertTrue(evaluate(markdown, {"type": "regex", "value": r"CAT\s*\|\s*8"}))
        self.assertTrue(evaluate(markdown, {"type": "min_chars", "value": 20}))
        self.assertTrue(evaluate(markdown, {"type": "min_table_rows", "value": 2}))
        self.assertTrue(evaluate(markdown, {"type": "not_contains", "value": "garbage"}))

    def test_normalization_ignores_marks_case_and_spacing(self):
        self.assertEqual(normalized("Intel®   PCH"), "intel pch")

    def test_structure_counts_and_containment(self):
        markdown = "# Title\n\n- one\n- two\n\n| A | B |\n| --- | --- |\n| 1 | 2 |"
        result = counts(markdown)
        self.assertEqual(result["headings"], 1)
        self.assertEqual(result["list_items"], 2)
        self.assertEqual(result["table_rows"], 2)
        grams = trigrams("one two three four")
        self.assertEqual(containment(grams, grams), 1.0)

    def test_hash_and_output_name_are_stable(self):
        with tempfile.TemporaryDirectory() as temp:
            path = Path(temp) / "sample.pdf"
            path.write_bytes(b"abc")
            self.assertEqual(
                sha256(path),
                "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
            )
            self.assertEqual(safe_name(path), "sample.pdf.md")


if __name__ == "__main__":
    unittest.main()
