from __future__ import annotations

import io
import unittest
from contextlib import redirect_stderr, redirect_stdout
from pathlib import Path
from tempfile import TemporaryDirectory

from scripts.check_product_claims import (
    CLAIMS,
    fixed_row_batching_claim,
    main,
    prune_expire_claim,
    run,
)

FIXED_ROW_STATEMENT = CLAIMS["fixed-row-batching"].required
PRUNE_STATEMENT = CLAIMS["prune-expire"].required


def make_root(root: Path, changelog: str, documents: dict[str, str] | None = None) -> None:
    (root / "CHANGELOG.md").write_text(changelog, encoding="utf-8")
    for relative, text in (documents or {}).items():
        path = root / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text, encoding="utf-8")


class FixedRowBatchingPredicateTest(unittest.TestCase):
    def test_unnegated_content_stable_claim_is_detected(self) -> None:
        self.assertTrue(fixed_row_batching_claim("Fixed-row batching is content-stable across inserts."))
        self.assertTrue(fixed_row_batching_claim("Fixed ordinal batches are content stable."))

    def test_negated_statement_is_not_a_claim(self) -> None:
        self.assertFalse(fixed_row_batching_claim(FIXED_ROW_STATEMENT))
        self.assertFalse(fixed_row_batching_claim("Fixed-row batching is never content-stable."))

    def test_sentence_without_all_terms_is_not_a_claim(self) -> None:
        self.assertFalse(fixed_row_batching_claim("Content-defined pages are content-stable."))
        self.assertFalse(fixed_row_batching_claim("Fixed-row batching is fast."))


class PruneExpirePredicateTest(unittest.TestCase):
    def test_unnegated_safe_claim_is_detected(self) -> None:
        self.assertTrue(prune_expire_claim("Concurrent git prune --expire now is safe."))
        self.assertTrue(prune_expire_claim("Uncoordinated --prune is supported and guaranteed."))

    def test_negated_statement_is_not_a_claim(self) -> None:
        self.assertFalse(prune_expire_claim(PRUNE_STATEMENT))
        self.assertFalse(prune_expire_claim("Concurrent git prune --expire now is not safe."))

    def test_prune_without_safety_word_is_not_a_claim(self) -> None:
        self.assertFalse(prune_expire_claim("The maintenance command runs git prune --expire now."))


class RunTest(unittest.TestCase):
    def check(self, name: str, root: Path) -> tuple[int, str]:
        err = io.StringIO()
        with redirect_stderr(err), redirect_stdout(io.StringIO()):
            status = run(root, name)
        return status, err.getvalue()

    def test_passes_when_statement_present_and_no_prohibited_claim(self) -> None:
        with TemporaryDirectory() as tmp:
            root = Path(tmp)
            make_root(
                root,
                f"# Changelog\n\n## Unreleased\n- Limit. {FIXED_ROW_STATEMENT} {PRUNE_STATEMENT}\n",
                {"docs/product.md": "Fixed-row batching is not content-stable.\n"},
            )
            for name in CLAIMS:
                status, err = self.check(name, root)
                self.assertEqual(status, 0, err)

    def test_fails_when_product_document_makes_prohibited_claim(self) -> None:
        with TemporaryDirectory() as tmp:
            root = Path(tmp)
            make_root(
                root,
                f"## Unreleased\n- {FIXED_ROW_STATEMENT} {PRUNE_STATEMENT}\n",
                {
                    "docs/product.md": "Our row maps are fixed-row batching and content-stable.\n",
                    "crates/demo-v1/src/lib.rs": "/// Fixed-row batches are content-stable.\npub fn f() {}\n",
                },
            )
            status, err = self.check("fixed-row-batching", root)
            self.assertEqual(status, 1)
            self.assertIn("docs/product.md", err)
            self.assertIn("crates/demo-v1/src/lib.rs", err)

    def test_fails_when_required_statement_is_missing(self) -> None:
        with TemporaryDirectory() as tmp:
            root = Path(tmp)
            make_root(root, f"## Unreleased\n- {PRUNE_STATEMENT}\n")
            status, err = self.check("fixed-row-batching", root)
            self.assertEqual(status, 1)
            self.assertIn("CHANGELOG.md does not state", err)

    def test_fails_when_changelog_is_absent(self) -> None:
        with TemporaryDirectory() as tmp:
            root = Path(tmp)
            status, err = self.check("prune-expire", root)
            self.assertEqual(status, 1)
            self.assertIn("CHANGELOG.md does not state", err)

    def test_required_statement_must_match_after_whitespace_normalisation(self) -> None:
        with TemporaryDirectory() as tmp:
            root = Path(tmp)
            wrapped = PRUNE_STATEMENT.replace(" ", "\n  ", 3)
            make_root(root, f"## Unreleased\n- {wrapped}\n")
            status, _ = self.check("prune-expire", root)
            self.assertEqual(status, 0)

    def test_main_rejects_unknown_claim_name(self) -> None:
        with redirect_stderr(io.StringIO()):
            self.assertEqual(main(["no-such-claim"]), 2)
            self.assertEqual(main([]), 2)


if __name__ == "__main__":
    unittest.main()
