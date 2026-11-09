#!/usr/bin/env python3
"""Enforce the OrnaDB crate layer table and the size budgets as a ratchet.

The crate graph is read from `cargo metadata`, never from a hand-written list, so
a new dependency cannot escape the check. The policy is ARCHITECTURE.md; the
machine-readable layer table is docs/design/layers.toml and the recorded size
debt is scripts/architecture-budget.toml.

Modes:

    --mode=report   print the current state, including every known violation,
                    and exit 0. This is the mode the `arch` CI job uses to show
                    the debt a stage has not fixed yet.
    --mode=deny     exit 1 on a violation that is not recorded in the `known`
                    list of layers.toml. Recorded violations are the ratchet
                    baseline and do not fail the job. With --strict the baseline
                    is ignored, which proves the check still has teeth.
    --mode=budget   exit 1 on new debt: a file or function over its hard cap
                    with no recorded entry or reviewed raise, or a recorded
                    entry growing by more than max(5%, 25 lines). Shrinkage and
                    growth inside that tolerance are non-failing notes, so a PR
                    that is behind a merged refactor is never failed by another
                    PR's good news. There is no crate-level line budget.

With `--mode=budget --update` the budget file is rewritten downwards: entries that
shrank are lowered or removed, and an entry that grew is left unchanged. The
update never raises or adds an entry. A deliberate raise is a reviewed [[raise]]
in `scripts/architecture-budget-raises.toml` (path, new_limit, reason, bead); the
raise is the visible exception and the budget file only moves down. The CI job
never passes --update.

The rules, in the order ARCHITECTURE.md states them:

    R1  no optional internal dependency; an internal edge is production or absent
    R2  the shipped binary never links the harness, a tool, the oracle or the
        archive
    R3  naming (checked in review, not here)
    layer table  a crate may depend only on a lower layer; a same-layer edge is
                 legal only when the source crate's `allowed` list names it
    archive      no crate may depend on a layer-15 archive crate
    orphan       a crate with production in-degree 0 must be declared a tool or
                 an archive, or be the shipped binary
    tool         a crate outside the shipped closure must not depend on a
                 non-production crate

Exit status: 0 when the requested mode passes, 1 when it fails its policy, 2 when
the configuration or `cargo metadata` cannot be read.
"""

from __future__ import annotations

import argparse
import bisect
import json
import re
import subprocess
import sys
import tomllib
from collections import Counter
from dataclasses import dataclass
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
LAYERS_PATH = Path("docs/design/layers.toml")
BUDGET_PATH = Path("scripts/architecture-budget.toml")
RAISES_PATH = Path("scripts/architecture-budget-raises.toml")
# Archived crates sit one group directory deeper than live crates.
ARCHIVE_DIRECTORY = "archive"

BUDGET_HEADER = """\
# Size budgets for the OrnaDB workspace.
#
# Enforced by `python3 scripts/check_architecture.py --mode=budget` in the
# `arch` job of .github/workflows/lint.yml. ARCHITECTURE.md is the policy;
# this file is the recorded debt.
#
# Only per-file and per-function entries are recorded. There is no crate-level
# line budget: adding a few types to a large crate is not a violation.
#
# `--mode=budget` fails when a file or function appears over its hard cap with
# no entry, or when a recorded entry grows by more than max(5%, 25 lines).
# Shrinkage and growth inside that tolerance are non-failing notes, so a PR
# that is behind a merged refactor is never failed by another PR's good news.
#
# Lower the drift after a merge with:
#
#   python3 scripts/check_architecture.py --mode=budget --update
#
# It lowers or removes entries that shrank and never raises one.
#
# A deliberate raise is not written here: add or update a reviewed [[raise]] in
# scripts/architecture-budget-raises.toml with a reason and a bead. That file is
# the visible exception list; this file only ever moves down.
#
#   file_lines       hard cap on one source file; a file above it needs an
#                    explicit files entry or a raise
#   function_lines   hard cap on one function; a function above it needs an
#                    explicit functions entry or a raise
#"""

BUDGET_KEYS = ("limits", "crate")


ROLES = ("production", "tool", "harness", "oracle", "archive")
NON_PRODUCTION_ROLES = frozenset(ROLES) - {"production"}


class ConfigError(Exception):
    """The layer table, the budget file or cargo metadata is unusable."""


@dataclass(frozen=True)
class CrateSpec:
    name: str
    layer: int
    allowed: tuple[str, ...]
    role: str
    warning: str


@dataclass(frozen=True)
class Edge:
    src: str
    dst: str
    kind: str  # "production" or "optional"
    manifest: Path
    alias: str


@dataclass
class Workspace:
    root: Path
    members: dict[str, Path]  # crate name -> crate directory
    edges: list[Edge]
    optional: list[Edge]


@dataclass(frozen=True)
class Violation:
    key: str
    code: str
    message: str
    id: str | None = None


@dataclass(frozen=True)
class WarningEntry:
    key: str
    message: str
    id: str | None = None


@dataclass
class LayerTable:
    crates: dict[str, CrateSpec]
    shipped_binary: str
    forbidden_in_shipped: tuple[str, ...]
    known: dict[str, dict]


@dataclass(frozen=True)
class CrateMeasure:
    src_lines: int
    allow_clippy: int
    files: dict[str, int]
    functions: dict[str, int]


# --------------------------------------------------------------------------
# Configuration
# --------------------------------------------------------------------------


def load_layer_table(path: Path) -> LayerTable:
    try:
        data = tomllib.loads(path.read_text(encoding="utf-8"))
    except OSError as error:
        raise ConfigError(f"cannot read layer table {path}: {error}") from error
    except tomllib.TOMLDecodeError as error:
        raise ConfigError(f"invalid TOML in {path}: {error}") from error

    meta = data.get("meta")
    if not isinstance(meta, dict):
        raise ConfigError(f"{path}: missing [meta] table")
    shipped = meta.get("shipped_binary")
    if not isinstance(shipped, str) or not shipped:
        raise ConfigError(f"{path}: [meta] shipped_binary must name a crate")
    forbidden = meta.get("forbidden_in_shipped", [])
    if not isinstance(forbidden, list) or any(not isinstance(n, str) for n in forbidden):
        raise ConfigError(f"{path}: [meta] forbidden_in_shipped must be a list of crate names")

    crates: dict[str, CrateSpec] = {}
    for raw in data.get("crate", []):
        if not isinstance(raw, dict):
            raise ConfigError(f"{path}: every [[crate]] entry must be a table")
        name = raw.get("name")
        layer = raw.get("layer")
        if not isinstance(name, str) or not name:
            raise ConfigError(f"{path}: [[crate]] entry without a name")
        if name in crates:
            raise ConfigError(f"{path}: crate {name} is listed twice")
        if not isinstance(layer, int) or isinstance(layer, bool) or layer < 0:
            raise ConfigError(f"{path}: crate {name} has no non-negative integer layer")
        allowed = raw.get("allowed", [])
        if not isinstance(allowed, list) or any(not isinstance(n, str) for n in allowed):
            raise ConfigError(f"{path}: crate {name} allowed must be a list of crate names")
        role = raw.get("role", "production")
        if role not in ROLES:
            raise ConfigError(f"{path}: crate {name} has unknown role {role!r}")
        warning = raw.get("warning")
        if warning is not None and not isinstance(warning, str):
            raise ConfigError(f"{path}: crate {name} warning must be a string")
        crates[name] = CrateSpec(name, layer, tuple(allowed), role, warning or "")

    known: dict[str, dict] = {}
    for raw in data.get("known", []):
        if not isinstance(raw, dict):
            raise ConfigError(f"{path}: every [[known]] entry must be a table")
        key = raw.get("key")
        if not isinstance(key, str) or not key:
            raise ConfigError(f"{path}: [[known]] entry without a key")
        if key in known:
            raise ConfigError(f"{path}: known violation {key} is listed twice")
        known[key] = raw

    if shipped not in crates:
        raise ConfigError(f"{path}: shipped binary {shipped} is not in the layer table")

    return LayerTable(crates, shipped, tuple(forbidden), known)


def load_budget(path: Path) -> dict:
    try:
        data = tomllib.loads(path.read_text(encoding="utf-8"))
    except OSError as error:
        raise ConfigError(f"cannot read budget {path}: {error}") from error
    except tomllib.TOMLDecodeError as error:
        raise ConfigError(f"invalid TOML in {path}: {error}") from error

    limits = data.get("limits")
    if not isinstance(limits, dict):
        raise ConfigError(f"{path}: missing [limits] table")
    for key in ("file_lines", "function_lines"):
        value = limits.get(key)
        if not isinstance(value, int) or isinstance(value, bool) or value <= 0:
            raise ConfigError(f"{path}: [limits] {key} must be a positive integer")
    crates: dict[str, dict] = {}
    for raw in data.get("crate", []):
        if not isinstance(raw, dict):
            raise ConfigError(f"{path}: every [[crate]] entry must be a table")
        name = raw.get("name")
        if not isinstance(name, str) or not name:
            raise ConfigError(f"{path}: [[crate]] entry without a name")
        if name in crates:
            raise ConfigError(f"{path}: crate {name} is listed twice")
        entry: dict = {"files": {}, "functions": {}}
        for table in ("files", "functions"):
            entries = raw.get(table, {})
            if not isinstance(entries, dict):
                raise ConfigError(f"{path}: crate {name} {table} must be a table")
            for item, value in entries.items():
                if not isinstance(value, int) or isinstance(value, bool) or value <= 0:
                    raise ConfigError(f"{path}: crate {name} {table} {item!r} must be a positive integer")
            entry[table] = dict(entries)
        if entry["files"] or entry["functions"]:
            crates[name] = entry
    return {"limits": limits, "crates": crates}


def parse_raise_path(path: Path, text: str) -> tuple[str, str, str]:
    """Split `crates/[archive/]<crate>/<relative>[::<function>]` into its budget key."""
    if not isinstance(text, str) or not text:
        raise ConfigError(f"{path}: every [[raise]] needs a path")
    if "::" in text:
        prefix, key = text.split("::", 1)
        kind = "functions"
        if not key:
            raise ConfigError(f"{path}: raise path {text!r} has an empty function name")
    else:
        prefix, key = text, ""
        kind = "files"
    parts = prefix.split("/")
    if len(parts) < 3 or parts[0] != "crates" or not parts[1]:
        raise ConfigError(f"{path}: raise path {text!r} must start with crates/<crate>/")
    if parts[1] == ARCHIVE_DIRECTORY:
        if len(parts) < 4 or not parts[2]:
            raise ConfigError(f"{path}: raise path {text!r} must name the archived crate")
        crate = parts[2]
        relative = "/".join(parts[3:])
    else:
        crate = parts[1]
        relative = "/".join(parts[2:])
    if not relative:
        raise ConfigError(f"{path}: raise path {text!r} has no file part")
    if kind == "functions":
        return crate, kind, f"{relative}::{key}"
    return crate, kind, relative


def load_raises(path: Path) -> dict[tuple[str, str, str], dict]:
    if not path.exists():
        return {}
    try:
        data = tomllib.loads(path.read_text(encoding="utf-8"))
    except OSError as error:
        raise ConfigError(f"cannot read raises {path}: {error}") from error
    except tomllib.TOMLDecodeError as error:
        raise ConfigError(f"invalid TOML in {path}: {error}") from error

    raises: dict[tuple[str, str, str], dict] = {}
    for raw in data.get("raise", []):
        if not isinstance(raw, dict):
            raise ConfigError(f"{path}: every [[raise]] entry must be a table")
        text = raw.get("path")
        key = parse_raise_path(path, text)
        new_limit = raw.get("new_limit")
        if not isinstance(new_limit, int) or isinstance(new_limit, bool) or new_limit <= 0:
            raise ConfigError(f"{path}: raise {text!r} new_limit must be a positive integer")
        for field in ("reason", "bead"):
            value = raw.get(field)
            if not isinstance(value, str) or not value.strip():
                raise ConfigError(f"{path}: raise {text!r} needs a non-empty {field}")
        if key in raises:
            raise ConfigError(f"{path}: raise path {text!r} is listed twice")
        raises[key] = {
            "path": text,
            "new_limit": new_limit,
            "reason": raw["reason"],
            "bead": raw["bead"],
        }
    return raises


# --------------------------------------------------------------------------
# cargo metadata
# --------------------------------------------------------------------------


def load_metadata(root: Path, metadata_path: Path | None) -> dict:
    if metadata_path is not None:
        try:
            return json.loads(metadata_path.read_text(encoding="utf-8"))
        except OSError as error:
            raise ConfigError(f"cannot read metadata {metadata_path}: {error}") from error
        except json.JSONDecodeError as error:
            raise ConfigError(f"invalid JSON in {metadata_path}: {error}") from error
    try:
        done = subprocess.run(
            ["cargo", "metadata", "--format-version", "1", "--locked"],
            cwd=root,
            check=True,
            capture_output=True,
            text=True,
        )
    except FileNotFoundError as error:
        raise ConfigError("cargo is not on PATH") from error
    except subprocess.CalledProcessError as error:
        detail = (error.stderr or error.stdout or "").strip()
        raise ConfigError(f"cargo metadata failed: {detail}") from error
    try:
        return json.loads(done.stdout)
    except json.JSONDecodeError as error:
        raise ConfigError(f"cargo metadata returned invalid JSON: {error}") from error


def collect_workspace(root: Path, metadata: dict) -> Workspace:
    member_ids = set(metadata.get("workspace_members", []))
    packages = [p for p in metadata.get("packages", []) if p.get("id") in member_ids]
    if not packages:
        raise ConfigError("cargo metadata reports no workspace members")
    member_names = {p["name"] for p in packages}
    members: dict[str, Path] = {}
    edges: list[Edge] = []
    optional: list[Edge] = []
    for package in packages:
        name = package["name"]
        manifest = Path(package["manifest_path"])
        if not manifest.is_absolute():
            manifest = root / manifest
        members[name] = manifest.parent
        seen: set[tuple[str, str]] = set()
        for dep in package.get("dependencies", []):
            target = dep.get("name")
            if target not in member_names:
                continue
            kind = dep.get("kind")
            is_optional = bool(dep.get("optional"))
            if is_optional and kind in (None, "build"):
                edge_kind = "optional"
            elif kind is None:
                edge_kind = "production"
            else:
                continue
            if (target, edge_kind) in seen:
                continue
            seen.add((target, edge_kind))
            edge = Edge(name, target, edge_kind, manifest, dep.get("rename") or target)
            if edge_kind == "optional":
                optional.append(edge)
            else:
                edges.append(edge)
    return Workspace(root, members, edges, optional)


# --------------------------------------------------------------------------
# Layer checks
# --------------------------------------------------------------------------


def closure(workspace: Workspace, start: str) -> set[str]:
    reached = {start}
    pending = [start]
    by_src: dict[str, list[str]] = {}
    for edge in workspace.edges:
        by_src.setdefault(edge.src, []).append(edge.dst)
    while pending:
        current = pending.pop()
        for target in by_src.get(current, []):
            if target not in reached:
                reached.add(target)
                pending.append(target)
    return reached


def display_path(path: Path, root: Path) -> str:
    try:
        return path.relative_to(root).as_posix()
    except ValueError:
        return str(path)


def dependency_location(manifest: Path, alias: str, root: Path) -> str:
    location = display_path(manifest, root)
    try:
        lines = manifest.read_text(encoding="utf-8").splitlines()
    except OSError:
        return location
    section = ""
    for number, raw in enumerate(lines, 1):
        line = raw.strip()
        if line.startswith("[") and line.endswith("]"):
            section = line.strip("[]").strip()
            continue
        table = section.split(".")[-1].strip("'\"")
        if table not in ("dependencies", "dev-dependencies", "build-dependencies"):
            continue
        if not line or line.startswith("#") or "=" not in line:
            continue
        key = line.split("=", 1)[0].strip().strip("'\"")
        if key == alias:
            return f"{location}:{number}"
    return location


def check_layers(workspace: Workspace, table: LayerTable) -> tuple[list[Violation], list[WarningEntry]]:
    violations: list[Violation] = []
    warnings: list[WarningEntry] = []

    for name in sorted(workspace.members):
        if name not in table.crates:
            manifest = display_path(workspace.members[name] / "Cargo.toml", workspace.root)
            violations.append(Violation(
                f"missing-crate:{name}",
                "TABLE",
                f"{name}: workspace member missing from {LAYERS_PATH} ({manifest})",
            ))
    for name in sorted(table.crates):
        if name not in workspace.members:
            violations.append(Violation(
                f"unknown-crate:{name}",
                "TABLE",
                f"{name}: named in {LAYERS_PATH} but not a workspace member",
            ))

    same_layer_seen: dict[str, set[str]] = {}
    for edge in workspace.edges:
        src = table.crates.get(edge.src)
        dst = table.crates.get(edge.dst)
        if src is None or dst is None:
            continue
        location = dependency_location(edge.manifest, edge.alias, workspace.root)
        if dst.role == "archive":
            violations.append(Violation(
                f"archive:{edge.src}->{edge.dst}",
                "ARCHIVE",
                f"{edge.src} -> {edge.dst}: nothing may depend on an archive crate ({location})",
            ))
            continue
        if dst.layer > src.layer:
            violations.append(Violation(
                f"layer:{edge.src}->{edge.dst}",
                "LAYER",
                f"{edge.src} -> {edge.dst}: layer {src.layer} -> {dst.layer}, upward ({location})",
            ))
        elif dst.layer == src.layer:
            same_layer_seen.setdefault(edge.src, set()).add(edge.dst)
            if edge.dst not in src.allowed:
                violations.append(Violation(
                    f"same-layer:{edge.src}->{edge.dst}",
                    "LAYER",
                    f"{edge.src} -> {edge.dst}: same layer {src.layer} but not in "
                    f"{edge.src}'s allowed list ({location})",
                ))

    for edge in workspace.optional:
        src = table.crates.get(edge.src)
        dst = table.crates.get(edge.dst)
        if src is None or dst is None:
            continue
        location = dependency_location(edge.manifest, edge.alias, workspace.root)
        violations.append(Violation(
            f"optional:{edge.src}->{edge.dst}",
            "R1",
            f"{edge.src} -> {edge.dst}: optional internal dependency, "
            f"feature-gated internal edges are forbidden ({location})",
        ))

    for name, spec in table.crates.items():
        for allowed in spec.allowed:
            if allowed not in workspace.members:
                violations.append(Violation(
                    f"unknown-allowed:{name}->{allowed}",
                    "TABLE",
                    f"{name}: allowed list names {allowed}, which is not a workspace member",
                ))
            elif allowed not in same_layer_seen.get(name, set()):
                warnings.append(WarningEntry(
                    f"stale-allowed:{name}->{allowed}",
                    f"{name}: allowed entry {allowed} is not a current same-layer dependency; remove it",
                ))

    production_in = Counter(edge.dst for edge in workspace.edges)
    for name in sorted(workspace.members):
        if production_in[name] or name == table.shipped_binary:
            continue
        spec = table.crates.get(name)
        if spec is not None and spec.role in ("tool", "archive"):
            continue
        violations.append(Violation(
            f"orphan:{name}",
            "ORPHAN",
            f"{name}: production in-degree 0 and role is not tool or archive "
            f"(declare a role or give the crate a production dependent) "
            f"({display_path(workspace.members[name] / 'Cargo.toml', workspace.root)})",
        ))

    shipped = closure(workspace, table.shipped_binary)
    for edge in workspace.edges:
        if edge.src not in shipped:
            continue
        if edge.dst not in workspace.members:
            continue
        dst = table.crates.get(edge.dst)
        role = dst.role if dst is not None else "production"
        if role == "archive":
            continue
        if role in NON_PRODUCTION_ROLES or edge.dst in table.forbidden_in_shipped:
            location = dependency_location(edge.manifest, edge.alias, workspace.root)
            violations.append(Violation(
                f"shipped:{edge.src}->{edge.dst}",
                "R2",
                f"{edge.src} -> {edge.dst}: {role} crate in the shipped binary's closure ({location})",
            ))

    for edge in workspace.edges:
        if edge.src in shipped:
            continue
        dst = table.crates.get(edge.dst)
        src = table.crates.get(edge.src)
        if dst is None or src is None or dst.role == "archive":
            continue
        if dst.role in ("harness", "oracle", "archive"):
            location = dependency_location(edge.manifest, edge.alias, workspace.root)
            violations.append(Violation(
                f"tool:{edge.src}->{edge.dst}",
                "TOOL",
                f"{edge.src} -> {edge.dst}: standalone crate depends on {dst.role} crate ({location})",
            ))

    unique: dict[str, Violation] = {}
    for violation in violations:
        unique.setdefault(violation.key, violation)
    number = 0
    for spec in table.crates.values():
        if spec.warning:
            number += 1
            warnings.append(WarningEntry(
                f"warning:{spec.name}", f"{spec.name}: {spec.warning}", f"W{number}"
            ))
    return list(unique.values()), warnings


# --------------------------------------------------------------------------
# Source measurement
# --------------------------------------------------------------------------

IDENT = re.compile(r"[A-Za-z0-9_]")
FUNCTION = re.compile(
    r"(?m)^[ \t]*(?:pub(?:\([^)\n]*\))?[ \t]+)?(?:default[ \t]+)?(?:const[ \t]+)?"
    r"(?:async[ \t]+)?(?:unsafe[ \t]+)?(?:extern[ \t]+)?"
    r"fn[ \t]+(?P<name>[A-Za-z_][A-Za-z0-9_]*)"
)
ALLOW_CLIPPY = re.compile(r"#!?\[allow\([^\]\n]*clippy::")


def strip_rust(text: str) -> str:
    """Blank comments and string/char literals while preserving offsets and newlines.

    Brace matching runs on the result, so a `}` inside a string or a doc comment
    cannot be mistaken for the end of a function body.
    """
    out = list(text)
    length = len(text)

    def blank(start: int, stop: int) -> None:
        for index in range(start, min(stop, length)):
            if out[index] != "\n":
                out[index] = " "

    index = 0
    while index < length:
        char = text[index]
        if char == "/" and text.startswith("//", index):
            stop = text.find("\n", index)
            stop = length if stop < 0 else stop
            blank(index, stop)
            index = stop
            continue
        if char == "/" and text.startswith("/*", index):
            depth = 0
            stop = index
            while stop < length:
                if text.startswith("/*", stop):
                    depth += 1
                    stop += 2
                elif text.startswith("*/", stop):
                    depth -= 1
                    stop += 2
                    if depth == 0:
                        break
                else:
                    stop += 1
            blank(index, stop)
            index = stop
            continue
        boundary = index == 0 or not (IDENT.match(text[index - 1]) or text[index - 1] == "_")
        if boundary and (char == "r" or (char in "bc" and index + 1 < length and text[index + 1] == "r")):
            cursor = index + (1 if char == "r" else 2)
            hashes = 0
            while cursor < length and text[cursor] == "#":
                hashes += 1
                cursor += 1
            if cursor < length and text[cursor] == '"':
                terminator = '"' + "#" * hashes
                stop = text.find(terminator, cursor + 1)
                stop = length if stop < 0 else stop + len(terminator)
                blank(index, stop)
                index = stop
                continue
        if boundary and char in "bc" and index + 1 < length and text[index + 1] == '"':
            char = '"'
            index += 1
        if char == '"':
            cursor = index + 1
            while cursor < length:
                if text[cursor] == "\\":
                    cursor += 2
                    continue
                if text[cursor] == '"':
                    cursor += 1
                    break
                cursor += 1
            blank(index, cursor)
            index = cursor
            continue
        if char == "'":
            cursor = index + 1
            if cursor < length and text[cursor] == "\\":
                cursor += 1
                while cursor < length and text[cursor] != "'":
                    cursor += 1
                if cursor < length:
                    cursor += 1
                blank(index, cursor)
                index = cursor
                continue
            if cursor + 1 < length and text[cursor + 1] == "'":
                blank(index, index + 3)
                index += 3
                continue
        index += 1
    return "".join(out)


def scan_functions(text: str) -> list[tuple[str, int]]:
    """Return (name, line count) for every function with a body, in file order."""
    stripped = strip_rust(text)
    line_starts = [0]
    for index, char in enumerate(stripped):
        if char == "\n":
            line_starts.append(index + 1)

    def line_of(offset: int) -> int:
        return bisect.bisect_right(line_starts, offset)

    found: list[tuple[str, int]] = []
    for match in FUNCTION.finditer(stripped):
        body = stripped.find("{", match.end())
        if body < 0:
            continue
        semicolon = stripped.find(";", match.end())
        if 0 <= semicolon < body:
            continue
        depth = 0
        end = -1
        for cursor in range(body, len(stripped)):
            char = stripped[cursor]
            if char == "{":
                depth += 1
            elif char == "}":
                depth -= 1
                if depth == 0:
                    end = cursor
                    break
        if end < 0:
            continue
        found.append((match.group("name"), line_of(end) - line_of(match.start()) + 1))
    return found


def measure_crate(directory: Path) -> CrateMeasure:
    source = directory / "src"
    files: dict[str, int] = {}
    functions: dict[str, int] = {}
    allow_clippy = 0
    if source.is_dir():
        for path in sorted(source.rglob("*.rs")):
            try:
                text = path.read_text(encoding="utf-8", errors="replace")
            except OSError:
                continue
            relative = path.relative_to(directory).as_posix()
            files[relative] = len(text.splitlines())
            allow_clippy += len(ALLOW_CLIPPY.findall(text))
            counts: Counter[str] = Counter()
            for name, lines in scan_functions(text):
                counts[name] += 1
                key = f"{relative}::{name}" if counts[name] == 1 else f"{relative}::{name}#{counts[name]}"
                functions[key] = lines
    return CrateMeasure(sum(files.values()), allow_clippy, files, functions)


def growth_tolerance(limit: int) -> int:
    """The growth a recorded entry may absorb before it needs a reviewed raise."""
    return max((limit * 5 + 99) // 100, 25)


def budget_limit(base: int | None, raise_spec: dict | None) -> int:
    values = [value for value in (base, raise_spec["new_limit"] if raise_spec else None) if value]
    return max(values) if values else 0


def check_budget(
    workspace: Workspace, budget: dict, raises: dict[tuple[str, str, str], dict]
) -> tuple[list[Violation], list[WarningEntry]]:
    violations: list[Violation] = []
    notes: list[WarningEntry] = []
    limits = budget["limits"]
    recorded = budget["crates"]

    for name in sorted(recorded):
        if name not in workspace.members:
            notes.append(WarningEntry(
                f"stale-budget:{name}",
                f"{name}: recorded in {BUDGET_PATH} but not a workspace member; "
                f"run --mode=budget --update",
            ))

    seen_raises: set[tuple[str, str, str]] = set()
    for name in sorted(workspace.members):
        measure = measure_crate(workspace.members[name])
        record = recorded.get(name, {})
        for kind, limit_key, measured, entries in (
            ("files", "file_lines", measure.files, record.get("files", {})),
            ("functions", "function_lines", measure.functions, record.get("functions", {})),
        ):
            cap = limits[limit_key]
            for key, now in sorted(measured.items()):
                base = entries.get(key)
                spec = raises.get((name, kind, key))
                if spec is not None:
                    seen_raises.add((name, kind, key))
                limit = budget_limit(base, spec)
                if limit == 0:
                    if now > cap:
                        violations.append(Violation(
                            f"budget:{name}:{key}",
                            "BUDGET",
                            f"{name}/{key}: {now} lines, cap {cap}; no recorded entry or reviewed raise. "
                            f"If this is a split child whose parent shrank, record it as a reviewed [[raise]] "
                            f"in {RAISES_PATH} (path, new_limit, reason, bead); otherwise split the file/function",
                        ))
                    continue
                tolerance = growth_tolerance(limit)
                if now > limit + tolerance:
                    origin = "reviewed raise" if spec is not None else "recorded entry"
                    violations.append(Violation(
                        f"budget:{name}:{key}",
                        "BUDGET",
                        f"{name}/{key}: {now} lines, {origin} {limit} (tolerance {tolerance}); "
                        f"add or update a [[raise]] in {RAISES_PATH} with a reason and a bead, "
                        f"or record the split child with the split's bead",
                    ))
                elif now > limit:
                    notes.append(WarningEntry(
                        f"growth:{name}:{key}",
                        f"{name}/{key}: {limit} -> {now} lines, within tolerance {tolerance}",
                    ))
                if spec is not None:
                    needed_above = (base + growth_tolerance(base)) if base is not None else cap
                    if now <= needed_above:
                        notes.append(WarningEntry(
                            f"unused-raise:{name}:{key}",
                            f"{name}/{key}: raise to {spec['new_limit']} is not needed "
                            f"(measured {now}); remove it from {RAISES_PATH}",
                        ))
            for key, old in sorted(entries.items()):
                if key not in measured:
                    notes.append(WarningEntry(
                        f"stale-budget:{name}:{key}",
                        f"{name}/{key}: recorded {old} lines but the file or function is gone; "
                        f"run --mode=budget --update",
                    ))
                elif measured[key] < old:
                    notes.append(WarningEntry(
                        f"shrink:{name}:{key}",
                        f"{name}/{key}: recorded {old} lines, now {measured[key]}; "
                        f"run --mode=budget --update",
                    ))

    for (crate, kind, key), spec in sorted(raises.items()):
        if crate not in workspace.members:
            notes.append(WarningEntry(
                f"unused-raise:{crate}:{kind}:{key}",
                f"{spec['path']}: raise names a crate that is not in the workspace; remove it",
            ))
        elif (crate, kind, key) not in seen_raises:
            notes.append(WarningEntry(
                f"unused-raise:{crate}:{kind}:{key}",
                f"{spec['path']}: raise path is not present in the workspace; "
                f"it is reserved for an in-flight PR and should be removed once stale",
            ))
    return violations, notes


# --------------------------------------------------------------------------
# Reporting
# --------------------------------------------------------------------------


def apply_baseline(
    violations: list[Violation], table: LayerTable
) -> tuple[list[Violation], list[WarningEntry]]:
    tagged: list[Violation] = []
    warnings: list[WarningEntry] = []
    present = {violation.key for violation in violations}
    for violation in violations:
        known = table.known.get(violation.key)
        tagged.append(violation if known is None else Violation(
            violation.key, violation.code, violation.message, known.get("id")
        ))
    for key, entry in table.known.items():
        if key not in present:
            warnings.append(WarningEntry(
                f"stale-known:{key}",
                f"{key}: recorded as known violation {entry.get('id', '?')} but no longer occurs; delete the entry",
            ))


    def sort_key(violation: Violation) -> tuple[int, str]:
        identifier = violation.id or ""
        if identifier.startswith("V") and identifier[1:].isdigit():
            return (0, f"{int(identifier[1:]):04d}")
        return (1, violation.key)

    return sorted(tagged, key=sort_key), warnings


def describe(violation: Violation) -> str:
    label = f"[{violation.id}]" if violation.id else "[new]"
    return f"{label} {violation.code:<8} {violation.message} [{violation.key}]"


def print_report(
    workspace: Workspace,
    table: LayerTable,
    violations: list[Violation],
    warnings: list[WarningEntry],
    budget_summary: str | None,
) -> None:
    print(
        f"architecture: {len(workspace.members)} crates, {len(workspace.edges)} production edges, "
        f"{len(workspace.optional)} optional internal edges"
    )
    print(
        f"layer table: {len(table.crates)} crates; shipped binary {table.shipped_binary}; "
        f"known violations {len(table.known)}"
    )
    known = sum(1 for violation in violations if violation.id)
    print(f"violations: {known} known, {len(violations) - known} new")
    for violation in violations:
        print(describe(violation))
    for warning in warnings:
        label = f"[{warning.id}]" if warning.id else "[warn]"
        print(f"{label} WARNING {warning.message}")
    if budget_summary is not None:
        print(budget_summary)


def render_budget(limits: dict, entries: list[dict]) -> str:
    lines = [BUDGET_HEADER.rstrip("\n"), "", "[limits]"]
    for key in ("file_lines", "function_lines"):
        lines.append(f"{key} = {limits[key]}")
    lines.append("")
    for entry in entries:
        lines.append("[[crate]]")
        lines.append(f'name = "{entry["name"]}"')
        if entry["files"]:
            lines.append("[crate.files]")
            for path, count in sorted(entry["files"].items()):
                lines.append(f'"{path}" = {count}')
        if entry["functions"]:
            lines.append("[crate.functions]")
            for key, count in sorted(entry["functions"].items()):
                lines.append(f'"{key}" = {count}')
        lines.append("")
    return "\n".join(lines).rstrip("\n") + "\n"


def update_budget(workspace: Workspace, budget: dict, path: Path) -> list[str]:
    """Rewrite the budget downwards; never raise or add an entry.

    Growth is not recorded here: check_budget reports it, and a deliberate raise
    goes in scripts/architecture-budget-raises.toml.
    """
    limits = budget["limits"]
    changes: list[str] = []
    entries: list[dict] = []
    for name, record in budget["crates"].items():
        if name not in workspace.members:
            changes.append(f"dropped crate entry {name}: not a workspace member")
            continue
        measure = measure_crate(workspace.members[name])
        entry: dict = {"name": name, "files": {}, "functions": {}}
        for kind, cap_key, measured, old_entries in (
            ("files", "file_lines", measure.files, record.get("files", {})),
            ("functions", "function_lines", measure.functions, record.get("functions", {})),
        ):
            cap = limits[cap_key]
            for key in sorted(old_entries):
                old = old_entries[key]
                if key not in measured:
                    changes.append(f"removed {name}/{key} (gone)")
                    continue
                now = measured[key]
                if now <= cap:
                    changes.append(f"removed {name}/{key} (now {now}, below cap {cap})")
                elif now > old:
                    entry[kind][key] = old
                else:
                    entry[kind][key] = now
                    if now < old:
                        changes.append(f"lowered {name}/{key} {old} -> {now}")
        if entry["files"] or entry["functions"]:
            entries.append(entry)
        elif record.get("files") or record.get("functions"):
            changes.append(f"removed empty crate entry {name}")
    path.write_text(render_budget(limits, entries), encoding="utf-8")
    return changes


def summarize_budget(budget: dict) -> str:
    crates = budget["crates"]
    recorded_files = sum(len(record.get("files", {})) for record in crates.values())
    recorded_functions = sum(len(record.get("functions", {})) for record in crates.values())
    return (
        f"budgets: {recorded_files} files over {budget['limits']['file_lines']} lines, "
        f"{recorded_functions} functions over {budget['limits']['function_lines']} lines "
        f"recorded in {BUDGET_PATH}"
    )


# --------------------------------------------------------------------------
# Entry point
# --------------------------------------------------------------------------


def run(args: argparse.Namespace) -> int:
    root = args.root.resolve()
    layers_path = args.layers if args.layers is not None else root / LAYERS_PATH
    budget_path = args.budget if args.budget is not None else root / BUDGET_PATH
    raises_path = args.raises if args.raises is not None else root / RAISES_PATH

    if args.update and args.mode != "budget":
        raise ConfigError("--update requires --mode=budget")

    table = load_layer_table(layers_path)
    metadata = load_metadata(root, args.metadata)
    workspace = collect_workspace(root, metadata)

    if args.mode == "budget":
        budget = load_budget(budget_path)
        raises = load_raises(raises_path)
        if args.update:
            changes = update_budget(workspace, budget, budget_path)
            if not changes:
                print("[note] no downward changes; every entry already matches or is above")
            for change in changes:
                print(f"[note] {change}")
            budget = load_budget(budget_path)
        violations, notes = check_budget(workspace, budget, raises)
        for violation in violations:
            print(describe(violation))
        for note in notes:
            print(f"[note] {note.message}")
        print(summarize_budget(budget))
        if violations:
            print(f"{len(violations)} budget violation(s): new debt or growth beyond a reviewed limit")
            return 1
        print("budgets: no new debt or over-limit growth")
        return 0

    violations, warnings = check_layers(workspace, table)
    violations, baseline_warnings = apply_baseline(violations, table)
    warnings.extend(baseline_warnings)

    valid_budget = budget_path.exists()
    budget_summary = None
    if valid_budget:
        budget_summary = summarize_budget(load_budget(budget_path))
    print_report(workspace, table, violations, warnings, budget_summary)

    if args.mode == "report":
        return 0

    new = [violation for violation in violations if violation.id is None]
    if args.strict:
        if violations:
            print(f"{len(violations)} architecture violation(s) with the baseline ignored (--strict)")
            return 1
        print("architecture: no violations (--strict)")
        return 0
    if new:
        print(f"{len(new)} new architecture violation(s): add none, or record the debt in {LAYERS_PATH}")
        return 1
    print(f"architecture: no new violations; {len(table.known)} known violation(s) remain recorded")
    return 0


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--mode", choices=("report", "deny", "budget"), default="report")
    parser.add_argument(
        "--strict",
        action="store_true",
        help="deny mode: ignore the known-violation baseline, so every violation fails",
    )
    parser.add_argument(
        "--update",
        action="store_true",
        help="budget mode: lower or remove entries that shrank; never raises an entry",
    )
    parser.add_argument("--root", type=Path, default=ROOT, help="workspace root (default: repository root)")
    parser.add_argument("--metadata", type=Path, help="cargo metadata JSON instead of running cargo")
    parser.add_argument("--layers", type=Path, help="layer table path (default: docs/design/layers.toml)")
    parser.add_argument("--budget", type=Path, help="budget path (default: scripts/architecture-budget.toml)")
    parser.add_argument(
        "--raises",
        type=Path,
        help="reviewed raise path (default: scripts/architecture-budget-raises.toml)",
    )
    return parser


def main(argv: list[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    try:
        return run(args)
    except ConfigError as error:
        print(f"check_architecture: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
