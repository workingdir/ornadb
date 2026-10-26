#!/usr/bin/env python3
"""Executable reference oracle for Orna editable-row key path vectors.

This is a small test helper, not the production implementation.  It deliberately
implements only the path-component algorithm owned by ORNA-PATH-001..012.
"""
from __future__ import annotations
from pathlib import Path
import argparse, json, re, sys

SAFE = frozenset(b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789._-")
HEX_LOWER = frozenset("0123456789abcdef")
DEVICE_BASES = {"con", "prn", "aux", "nul", "clock$", "conin$", "conout$"}
DEVICE_BASES |= {f"com{i}" for i in range(1, 10)}
DEVICE_BASES |= {f"lpt{i}" for i in range(1, 10)}

class PathCodecError(ValueError):
    pass


def ascii_lower(text: str) -> str:
    return "".join(chr(ord(c) + 32) if "A" <= c <= "Z" else c for c in text)


def portable_reserved(text: str) -> bool:
    """Repository-format-1 reserved predicate, independent of the host OS."""
    if text in {".", ".."}:
        return True
    trimmed = text.rstrip(" .")
    lowered = ascii_lower(trimmed)
    if lowered == ".git":
        return True
    first = lowered.split(".", 1)[0]
    return first in DEVICE_BASES


def encode_component(text: str) -> str:
    if text == "":
        return "~ff"
    raw = text.encode("utf-8")
    reserved = portable_reserved(text)
    trailing_dot_start = len(raw)
    while trailing_dot_start and raw[trailing_dot_start - 1] == 0x2E:
        trailing_dot_start -= 1
    out: list[str] = []
    for i, byte in enumerate(raw):
        force_escape = (reserved and i == 0) or (byte == 0x2E and i >= trailing_dot_start)
        if not force_escape and byte in SAFE and byte != 0x7E:
            out.append(chr(byte))
        else:
            out.append(f"~{byte:02x}")
    encoded = "".join(out)
    if len(encoded.encode("ascii")) > 200:
        raise PathCodecError("encoded component exceeds 200-byte limit")
    return encoded


def decode_component(encoded: str) -> str:
    try:
        encoded.encode("ascii")
    except UnicodeEncodeError as exc:
        raise PathCodecError("encoded component is not ASCII") from exc
    if encoded == "~ff":
        return ""
    if "~ff" in encoded:
        raise PathCodecError("empty-component marker is only valid as the complete component")
    out = bytearray()
    i = 0
    while i < len(encoded):
        ch = encoded[i]
        if ch == "~":
            if i + 2 >= len(encoded):
                raise PathCodecError("short escape")
            pair = encoded[i + 1 : i + 3]
            if len(pair) != 2 or any(c not in HEX_LOWER for c in pair):
                raise PathCodecError("escape must use two lowercase hexadecimal digits")
            out.append(int(pair, 16))
            i += 3
            continue
        byte = ord(ch)
        if byte not in SAFE or ch == "~":
            raise PathCodecError("raw byte is not permitted in an encoded component")
        out.append(byte)
        i += 1
    try:
        text = bytes(out).decode("utf-8")
    except UnicodeDecodeError as exc:
        raise PathCodecError("escaped bytes are not well-formed UTF-8") from exc
    if encode_component(text) != encoded:
        raise PathCodecError("non-canonical encoded component")
    return text


def collision_key(encoded: str) -> str:
    return ascii_lower(encoded)


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("vectors", nargs="?", type=Path, default=Path(__file__).resolve().parents[1] / "tests/path-vectors.json")
    args = ap.parse_args()
    data = json.loads(args.vectors.read_text(encoding="utf-8"))
    failures: list[str] = []

    for v in data["round_trip"]:
        value, expected = str(v["value"]), v["encoded"]
        try:
            actual = encode_component(value)
            if actual != expected:
                failures.append(f"encode {value!r}: {actual!r} != {expected!r}")
            decoded = decode_component(expected)
            if decoded != value:
                failures.append(f"decode {expected!r}: {decoded!r} != {value!r}")
        except Exception as exc:
            failures.append(f"round trip {value!r}: {exc}")

    for v in data["reject_encoded"]:
        try:
            decode_component(v["encoded"])
        except PathCodecError:
            pass
        else:
            failures.append(f"accepted invalid encoding {v['encoded']!r}")

    for v in data["portable_collisions"]:
        try:
            left = encode_component(v["left"])
            right = encode_component(v["right"])
        except Exception as exc:
            failures.append(f"collision vector {v}: {exc}")
            continue
        actual = collision_key(left)
        if actual != collision_key(right) or actual != v["collision_key"]:
            failures.append(f"collision vector mismatch {v}: {left!r}, {right!r}, {actual!r}")

    # Fixed reserved-name corpus samples beyond the prose's minimum vectors.
    extra = {
        "PRN": "~50RN",
        "com1.txt": "~63om1.txt",
        "LPT9.log": "~4cPT9.log",
        ".GIT": "~2eGIT",
        "git~1": "git~7e1",
    }
    for value, expected in extra.items():
        try:
            actual = encode_component(value)
            if actual != expected or decode_component(expected) != value:
                failures.append(f"reserved vector {value!r}: got {actual!r}, expected {expected!r}")
        except Exception as exc:
            failures.append(f"reserved vector {value!r}: {exc}")

    if failures:
        print("FAIL")
        for failure in failures:
            print(f"- {failure}")
        return 1
    print(f"PASS: {len(data['round_trip'])} round trips, {len(data['reject_encoded'])} rejections, {len(data['portable_collisions'])} collision pairs")
    return 0

if __name__ == "__main__":
    raise SystemExit(main())
