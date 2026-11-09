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
    --mode=budget   exit 1 when any recorded size number grows, or when a new
                    file, function or crate exceeds its hard cap. A recorded
                    number may only shrink.

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

ROLES = ("production", "tool", "harness", "oracle", "archive")
NON_PRODUCTION_ROLES = frozenset(ROLES) - {"production"}

BUDGET_KEYS = ("limits", "crate")


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
    for key in ("crate_src_lines", "file_lines", "function_lines"):
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
        for key in ("src_lines", "allow_clippy"):
            value = raw.get(key, 0)
            if not isinstance(value, int) or isinstance(value, bool) or value < 0:
                raise ConfigError(f"{path}: crate {name} {key} must be a non-negative integer")
        for table in ("files", "functions"):
            entries = raw.get(table, {})
            if not isinstance(entries, dict):
                raise ConfigError(f"{path}: crate {name} {table} must be a table")
            for entry, value in entries.items():
                if not isinstance(value, int) or isinstance(value, bool) or value <= 0:
                    raise ConfigError(f"{path}: crate {name} {table} {entry!r} must be a positive integer")
        crates[name] = raw
    return {"limits": limits, "crates": crates}


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


def check_budget(workspace: Workspace, budget: dict) -> tuple[list[Violation], list[WarningEntry]]:
    violations: list[Violation] = []
    warnings: list[WarningEntry] = []
    limits = budget["limits"]
    recorded = budget["crates"]

    for name in sorted(recorded):
        if name not in workspace.members:
            violations.append(Violation(
                f"budget:unknown-crate:{name}",
                "BUDGET",
                f"{name}: recorded in {BUDGET_PATH} but not a workspace member; remove the entry",
            ))

    for name in sorted(workspace.members):
        measure = measure_crate(workspace.members[name])
        record = recorded.get(name, {})
        if "src_lines" in record:
            if measure.src_lines > record["src_lines"]:
                violations.append(Violation(
                    f"budget:{name}:src_lines",
                    "BUDGET",
                    f"{name}: src grew to {measure.src_lines} lines, recorded {record['src_lines']}",
                ))
        elif measure.src_lines > limits["crate_src_lines"]:
            violations.append(Violation(
                f"budget:{name}:src_lines",
                "BUDGET",
                f"{name}: src has {measure.src_lines} lines, cap {limits['crate_src_lines']}; "
                f"no budget entry records the debt",
            ))

        allow_cap = record.get("allow_clippy", 0)
        if measure.allow_clippy > allow_cap:
            violations.append(Violation(
                f"budget:{name}:allow_clippy",
                "BUDGET",
                f"{name}: {measure.allow_clippy} #[allow(clippy::...)] attributes, recorded {allow_cap}",
            ))

        file_records = record.get("files", {})
        for relative, lines in sorted(measure.files.items()):
            if relative in file_records:
                if lines > file_records[relative]:
                    violations.append(Violation(
                        f"budget:{name}:{relative}",
                        "BUDGET",
                        f"{name}/{relative}: {lines} lines, recorded {file_records[relative]}",
                    ))
            elif lines > limits["file_lines"]:
                violations.append(Violation(
                    f"budget:{name}:{relative}",
                    "BUDGET",
                    f"{name}/{relative}: {lines} lines, cap {limits['file_lines']}; no budget entry records the debt",
                ))
        for relative, lines in sorted(file_records.items()):
            if relative not in measure.files:
                warnings.append(WarningEntry(
                    f"stale-budget:{name}:{relative}",
                    f"{name}/{relative}: recorded {lines} lines but the file is gone or empty; remove the entry",
                ))
            elif measure.files[relative] < lines:
                warnings.append(WarningEntry(
                    f"stale-budget:{name}:{relative}",
                    f"{name}/{relative}: recorded {lines} lines, now {measure.files[relative]}; lower the entry",
                ))

        function_records = record.get("functions", {})
        for key, lines in sorted(measure.functions.items()):
            if key in function_records:
                if lines > function_records[key]:
                    violations.append(Violation(
                        f"budget:{name}:{key}",
                        "BUDGET",
                        f"{name}/{key}: {lines} lines, recorded {function_records[key]}",
                    ))
            elif lines > limits["function_lines"]:
                violations.append(Violation(
                    f"budget:{name}:{key}",
                    "BUDGET",
                    f"{name}/{key}: {lines} lines, cap {limits['function_lines']}; no budget entry records the debt",
                ))
        for key, lines in sorted(function_records.items()):
            if key not in measure.functions:
                warnings.append(WarningEntry(
                    f"stale-budget:{name}:{key}",
                    f"{name}/{key}: recorded {lines} lines but the function is gone; remove the entry",
                ))
            elif measure.functions[key] < lines:
                warnings.append(WarningEntry(
                    f"stale-budget:{name}:{key}",
                    f"{name}/{key}: recorded {lines} lines, now {measure.functions[key]}; lower the entry",
                ))
    return violations, warnings


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


def summarize_budget(budget: dict) -> str:
    crates = budget["crates"]
    over_crates = sum(1 for record in crates.values() if "src_lines" in record)
    over_files = sum(len(record.get("files", {})) for record in crates.values())
    over_functions = sum(len(record.get("functions", {})) for record in crates.values())
    return (
        f"budgets: {over_crates} crates with a recorded src_lines entry, "
        f"{over_files} files over {budget['limits']['file_lines']} lines, "
        f"{over_functions} functions over {budget['limits']['function_lines']} lines "
        f"recorded in {BUDGET_PATH}"
    )


# --------------------------------------------------------------------------
# Entry point
# --------------------------------------------------------------------------


def run(args: argparse.Namespace) -> int:
    root = args.root.resolve()
    layers_path = args.layers if args.layers is not None else root / LAYERS_PATH
    budget_path = args.budget if args.budget is not None else root / BUDGET_PATH

    table = load_layer_table(layers_path)
    metadata = load_metadata(root, args.metadata)
    workspace = collect_workspace(root, metadata)

    if args.mode == "budget":
        budget = load_budget(budget_path)
        violations, warnings = check_budget(workspace, budget)
        for violation in violations:
            print(describe(violation))
        for warning in warnings:
            print(f"[warn] WARNING {warning.message}")
        print(summarize_budget(budget))
        if violations:
            print(f"{len(violations)} budget violation(s): a recorded number may only shrink")
            return 1
        print("budgets: no growth")
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
    parser.add_argument("--root", type=Path, default=ROOT, help="workspace root (default: repository root)")
    parser.add_argument("--metadata", type=Path, help="cargo metadata JSON instead of running cargo")
    parser.add_argument("--layers", type=Path, help="layer table path (default: docs/design/layers.toml)")
    parser.add_argument("--budget", type=Path, help="budget path (default: scripts/architecture-budget.toml)")
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
