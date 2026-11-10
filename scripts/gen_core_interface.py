"""Generate the engine-embedded `core` interface modules.

The source of truth for the `core` natives is the intrinsic export
(crates/orna-intrinsics/std_intrinsics.json). This script derives the body-less
`native fn` declarations for the root environment (crates/orna-hir/interface/core.orna)
and for the language-defined native groups (crates/orna-hir/interface/core/<module>.orna),
as decided in docs/design/hir.md section 4 and D12.

Defaults and effect classes that the export does not carry are checked-in tables
below; every value is cross-checked against the std wrapper it came from.

Usage:
    scripts/gen_core_interface.py            write the interface files
    scripts/gen_core_interface.py --check    exit 1 when a checked-in file is stale
    scripts/gen_core_interface.py --report   print the gap lists for the PR body
"""

import json
import pathlib
import re
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
from gen_interface import check_type_tokens, split_top_level  # noqa: E402

ROOT = pathlib.Path(__file__).resolve().parent.parent
EXPORT = ROOT / "crates/orna-intrinsics/std_intrinsics.json"
INTERFACE = ROOT / "crates/orna-hir/interface"

# Root environment rows (hir.md 2.6: `Some`, `now`, `uuid7`, `error` are core natives).
# `fail` stays an `IntrinsicForm` (abrupt evaluation rule), so it is not a native.
ROOT_ROWS = ["Some", "now", "uuid7", "error"]
ROOT_EXCLUDED = {"fail": "abrupt evaluation rule (hir.md 2.6 IntrinsicForm), not a native"}

# Language-defined native groups (hir.md section 4, `core.orna` row of the table).
INCLUDED_MODULES = [
    "std.collection",
    "std.text",
    "std.bits",
    "std.hash",
    "std.stats",
    "std.time",
    "std.encoding.json",
    "std.encoding.base64",
    "std.encoding.orna",
    "std.encoding.ovb",
]

# Built-in types that lowering resolves but the sys generator's list does not carry.
EXTRA_BUILTINS = {"Uuid", "Error"}

# Catalogue effect class -> Orna effect class. Pure, PinnedData and Stream reads are `none`
# (Main decision, csenl.17 review); Clock and Entropy map to the EffectKind of the same host read.
EFFECT_CLASS = {"Pure": "none", "PinnedData": "none", "Stream": "none", "Clock": "clock", "Entropy": "randomness"}

# Host-effect rows need a per-row class. None lies in INCLUDED_MODULES; the five Host rows
# (std.stream throttle/debounce, std.concurrent parallel/race/timeout) are reported as gaps.
HOST_TABLE = {}

# Default expressions, from the std wrapper of each row (stdlib/std). Keys: (module, row, param).
DEFAULTS = {
    ("std.collection", "window", "step"): "1",
    ("std.collection", "one", "predicate"): "value => true",
    ("std.stats", "mean", "scale"): "null",
    ("std.stats", "mean", "rounding"): "null",
    ("std.stats", "median", "scale"): "null",
    ("std.stats", "median", "rounding"): "null",
    ("std.stats", "percentile", "scale"): "null",
    ("std.stats", "percentile", "rounding"): "null",
    ("std.stats", "variance", "scale"): "null",
    ("std.stats", "variance", "rounding"): "null",
    ("std.stats", "standard_deviation", "scale"): "null",
    ("std.stats", "standard_deviation", "rounding"): "null",
    ("std.stats", "histogram", "include_final_upper"): "false",
    ("std.time", "resolve_local", "gap"): "null",
    ("std.time", "duration.compact.format", "context"): '{ locale: "en", time_zone: "UTC" }',
    ("std.time", "duration.clock.format", "context"): '{ locale: "en", time_zone: "UTC" }',
    ("std.time", "duration.words.format", "context"): '{ locale: "en", time_zone: "UTC" }',
    ("std.time", "duration.iso.format", "context"): '{ locale: "en", time_zone: "UTC" }',
    ("std.encoding.json", "decode_with_options", "ignore_unknown_fields"): "false",
}

# Export flags has_default but the std wrapper declares the parameter required; emitted as required.
REQUIRED_DESPITE_FLAG = {("std.collection", "bucket_by", "zone")}


def fail(message):
    sys.exit(f"gen_core_interface: {message}")


def native_name(label):
    return label.replace(".", "_")


def module_path(module):
    """`std.encoding.json` -> `encoding/json.orna`; `std.collection` -> `collection.orna`."""
    return "/".join(module.split(".")[1:]) + ".orna"


def orna_type(text, type_params, label):
    def substitute(match):
        index = int(match.group(1))
        if index >= len(type_params):
            fail(f"{label}: Param({index}) out of range for {type_params}")
        return type_params[index]

    return re.sub(r"Param\((\d+)\)", substitute, text)


def declaration(module, row, known_extra):
    label = row["name"]
    signature = row["signature"]
    type_params = signature["type_params"]
    generics = f"<{', '.join(type_params)}>" if type_params else ""
    known = set(type_params) | known_extra
    parameters = []
    required = 0
    for param in signature["params"]:
        name = param["name"]
        key = (module, label, name)
        text_type = orna_type(param["type"], type_params, f"{module}.{label}")
        check_type_tokens(text_type, known, f"{module}.{label}.{name}")
        if param["has_default"]:
            if key in REQUIRED_DESPITE_FLAG:
                required += 1
                parameters.append(f"{name}: {text_type}")
                continue
            if key not in DEFAULTS:
                fail(f"{module}.{label}: parameter `{name}` has a default but no default value is recorded")
            parameters.append(f"{name}: {text_type} = {DEFAULTS[key]}")
        else:
            if key in DEFAULTS:
                fail(f"{module}.{label}: default recorded for `{name}` but the export declares none")
            required += 1
            parameters.append(f"{name}: {text_type}")
    arities = signature["arities"]
    if arities:
        if arities[0] != required or arities[-1] != len(parameters):
            fail(f"{module}.{label}: arities {arities} disagree with {required} required / {len(parameters)} total")
    result = orna_type(signature["result"], type_params, f"{module}.{label}")
    check_type_tokens(result, known, f"{module}.{label} result")
    effect = row["effect"]
    if effect == "Host":
        if (module, label) not in HOST_TABLE:
            fail(f"{module}.{label}: Host effect has no per-row class")
        effect_class = HOST_TABLE[(module, label)]
    elif effect in EFFECT_CLASS:
        effect_class = EFFECT_CLASS[effect]
    else:
        fail(f"{module}.{label}: unknown effect `{effect}`")
    return [
        f"// {module}.{label}",
        f"pub native fn {native_name(label)}{generics}({', '.join(parameters)}): {result} effect {effect_class};",
        "",
    ]


def header(source_note):
    return (
        "// Generated by scripts/gen_core_interface.py from crates/orna-intrinsics/std_intrinsics.json.\n"
        "// Do not edit by hand. Regenerate with `python3 scripts/gen_core_interface.py`.\n"
        "//\n"
        f"// {source_note}\n"
    ).rstrip("\n")


def load_rows():
    export = json.loads(EXPORT.read_text(encoding="utf-8"))
    rows = export["intrinsics"]
    by_module = {}
    for row in rows:
        module = "std." + row["module"][len("std.") :] if row["module"].startswith("std.") else row["module"]
        by_module.setdefault(module, []).append(row)
    return by_module


def render():
    """Return ({path: text}, gaps)."""
    by_module = load_rows()
    gaps = {"excluded_root": [], "excluded_modules": [], "dispatch_source_only": [], "pipeline": [],
            "required_despite_flag": [], "singletons": [], "ambiguous_host": []}
    files = {}

    root_rows = {row["name"]: row for row in by_module.get("core", [])}
    for name in root_rows:
        if name not in ROOT_ROWS and name not in ROOT_EXCLUDED:
            fail(f"core row `{name}` is neither a root native nor excluded")
    for name, reason in ROOT_EXCLUDED.items():
        gaps["excluded_root"].append(f"core.{name}: {reason}")
    root_lines = [header("Root environment natives (hir.md 2.6, D12). Host-implemented; effects per EFFECT_CLASS."),
                  "// Engine-embedded `core` (D12): body-less `native fn` declarations are admitted only here",
                  "// and in `core/*.orna`. Re-exported by std where stable paths are wanted.", ""]
    for name in ROOT_ROWS:
        root_lines.extend(declaration("core", root_rows[name], {"Some"} | EXTRA_BUILTINS))
    files["core.orna"] = "\n".join(root_lines).rstrip() + "\n"

    for module in INCLUDED_MODULES:
        rows = by_module.get(module, [])
        native_rows = []
        for row in rows:
            if row["dispatch"]["kind"] != "Native":
                gaps["dispatch_source_only"].append(f"{module}.{row['name']} ({row['dispatch']['kind']})")
                continue
            native_rows.append(row)
            for param in row["signature"]["params"]:
                if param["pipeline"]:
                    gaps["pipeline"].append(f"{module}.{row['name']}.{param['name']}")
                if (module, row["name"], param["name"]) in REQUIRED_DESPITE_FLAG:
                    gaps["required_despite_flag"].append(f"{module}.{row['name']}.{param['name']}")
            if row["effect"] == "Host":
                gaps["ambiguous_host"].append(f"{module}.{row['name']}")
        if len(native_rows) == 1:
            gaps["singletons"].append(f"{module}.{native_rows[0]['name']}")
        lines = [header(f"Natives of {module} (language-defined, hir.md section 4)."), ""]
        for row in native_rows:
            lines.extend(declaration(module, row, EXTRA_BUILTINS))
        files[f"core/{module_path(module)}"] = "\n".join(lines).rstrip() + "\n"

    for module, rows in by_module.items():
        if module not in INCLUDED_MODULES and module != "core":
            gaps["excluded_modules"].append(f"{module} ({len(rows)} rows)")

    for path, text in files.items():
        if not text.isascii():
            fail(f"{path} is not plain ASCII")
    return files, gaps


def main(argv):
    files, gaps = render()
    if "--report" in argv:
        for key, items in gaps.items():
            print(f"{key} ({len(items)}):")
            for item in items:
                print(f"  {item}")
        return 0
    stale = []
    for path, text in files.items():
        target = INTERFACE / path
        if "--check" in argv:
            current = target.read_text(encoding="utf-8") if target.exists() else ""
            if current != text:
                stale.append(path)
        else:
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_text(text, encoding="utf-8")
    if stale:
        print(f"gen_core_interface: stale: {', '.join(stale)}; run scripts/gen_core_interface.py", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
