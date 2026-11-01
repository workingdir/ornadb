#!/usr/bin/env python3
"""Check that product documentation makes no prohibited acceptance claim.

Gate items C6 and D4 (scripts/gate-evidence-items.toml) name claims the product
must not make. Each check scans the product's prose (Markdown documents and
Rust doc comments; copies of the specification under tests/fixtures are not
product prose) for a sentence that makes the prohibited claim without a
negation, and requires the statement that replaces it to appear in CHANGELOG.md.

Usage: check_product_claims.py fixed-row-batching | prune-expire

Sentence detection is lexical. A claim split across a paragraph boundary, or
phrased without the words the predicate looks for, is not detected; the
required statement is what the product is held to.
"""

from __future__ import annotations

import re
import sys
from collections.abc import Callable, Iterable, Iterator
from dataclasses import dataclass
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

NEGATION = re.compile(r"\b(?:not|no|never|cannot|without|unsupported)\b|n't", re.IGNORECASE)
FIXED_ROW = re.compile(r"\bfixed[- ](?:row|ordinal)\b", re.IGNORECASE)
CONTENT_STABLE = re.compile(r"content[- ]stable", re.IGNORECASE)
BATCH = re.compile(r"\bbatch", re.IGNORECASE)
PRUNE = re.compile(r"\bprune\b|--expire\b|--prune\b", re.IGNORECASE)
ADVERTISED_SAFE = re.compile(r"\b(?:safe|safely|supported|guarantee[sd]?|advertis\w*)\b", re.IGNORECASE)
BLOCK_START = re.compile(r"(?:[-*+]\s|\d+\.\s|#|\||>)")
DOC_COMMENT = re.compile(r"^\s*//[/!] ?(.*)$")
SENTENCE_END = re.compile(r"(?<=[.!?;])\s+")


def fixed_row_batching_claim(sentence: str) -> bool:
    return bool(FIXED_ROW.search(sentence) and BATCH.search(sentence) and CONTENT_STABLE.search(sentence)) and not NEGATION.search(
        sentence
    )


def prune_expire_claim(sentence: str) -> bool:
    return bool(PRUNE.search(sentence) and ADVERTISED_SAFE.search(sentence)) and not NEGATION.search(sentence)


@dataclass(frozen=True)
class Claim:
    name: str
    prohibited: Callable[[str], bool]
    required: str


CLAIMS = {
    "fixed-row-batching": Claim(
        "fixed-row-batching",
        fixed_row_batching_claim,
        "Fixed-row batching is not content-stable: ORP row pages close at content-defined boundary anchors, "
        "with a maximum entry count as a backstop (`boundary_anchor` in `crates/orna-repository-v1/src/orp.rs`).",
    ),
    "prune-expire": Claim(
        "prune-expire",
        prune_expire_claim,
        "Concurrent uncoordinated `git prune --expire now` is unsupported and is not advertised as safe "
        "(ORNA-CAP-006); this build exposes no `prune` command.",
    ),
}


def product_documents(root: Path) -> list[Path]:
    paths = [root / "CHANGELOG.md", root / "README.md"]
    paths += sorted(root.glob("docs/**/*.md"))
    paths += sorted(root.glob("crates/*/README.md"))
    paths += sorted(root.glob("crates/*/src/**/*.rs"))
    return [path for path in paths if path.is_file()]


def prose_lines(path: Path) -> list[str]:
    lines = path.read_text(encoding="utf-8", errors="replace").splitlines()
    if path.suffix != ".rs":
        return lines
    return [match.group(1) if (match := DOC_COMMENT.match(line)) else "" for line in lines]


def paragraphs(lines: Iterable[str]) -> Iterator[str]:
    current: list[str] = []
    for line in lines:
        text = line.strip()
        if not text:
            if current:
                yield " ".join(current)
                current = []
            continue
        if current and BLOCK_START.match(text):
            yield " ".join(current)
            current = []
        current.append(text)
    if current:
        yield " ".join(current)


def sentences(paragraph: str) -> list[str]:
    return [part.strip() for part in SENTENCE_END.split(paragraph) if part.strip()]


def violations(root: Path, claim: Claim) -> tuple[list[str], int]:
    found: list[str] = []
    count = 0
    for path in product_documents(root):
        for paragraph in paragraphs(prose_lines(path)):
            for sentence in sentences(paragraph):
                count += 1
                if claim.prohibited(sentence):
                    found.append(f"{path.relative_to(root)}: prohibited claim: {sentence[:240]}")
    return found, count


def required_present(root: Path, claim: Claim) -> bool:
    changelog = root / "CHANGELOG.md"
    if not changelog.is_file():
        return False
    return claim.required in " ".join(changelog.read_text(encoding="utf-8").split())


def run(root: Path, name: str) -> int:
    claim = CLAIMS[name]
    found, count = violations(root, claim)
    for message in found:
        print(f"claim check {name}: {message}", file=sys.stderr)
    present = required_present(root, claim)
    if not present:
        print(f"claim check {name}: CHANGELOG.md does not state: {claim.required}", file=sys.stderr)
    if found or not present:
        return 1
    print(
        f"claim check {name}: passed; {count} sentences in {len(product_documents(root))} product documents "
        "make no prohibited claim and CHANGELOG.md states the required statement"
    )
    return 0


def main(argv: list[str]) -> int:
    if len(argv) != 1 or argv[0] not in CLAIMS:
        print(f"usage: check_product_claims.py {' | '.join(CLAIMS)}", file=sys.stderr)
        return 2
    return run(ROOT, argv[0])


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
