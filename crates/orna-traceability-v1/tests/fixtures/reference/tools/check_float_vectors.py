#!/usr/bin/env python3
"""Reference checks for Orna Float equality, total ordering and basic statistics vectors."""
from __future__ import annotations
from pathlib import Path
import argparse, json, math, struct

MASK = (1 << 64) - 1
CANONICAL_NAN = 0x7FF8000000000000


def f(bits: int) -> float:
    return struct.unpack('>d', bits.to_bytes(8, 'big'))[0]


def bits(value: float) -> int:
    return int.from_bytes(struct.pack('>d', value), 'big')


def total_key(value_bits: int) -> int:
    # Order-preserving transform for IEEE 754 totalOrder over binary64 bit patterns.
    if value_bits >> 63:
        return (~value_bits) & MASK
    return value_bits | (1 << 63)


def is_nan_bits(value_bits: int) -> bool:
    return math.isnan(f(value_bits))


def canonicalize_nan(value_bits: int) -> int:
    return CANONICAL_NAN if is_nan_bits(value_bits) else value_bits


def aggregate(op: str, values: list[int]) -> int:
    if any(is_nan_bits(v) for v in values):
        return CANONICAL_NAN
    floats = [f(v) for v in values]
    if op == 'min':
        result = min(floats)
        if result == 0.0 and any(v == 0x8000000000000000 for v in values):
            return 0x8000000000000000
        return bits(result)
    if op == 'max':
        result = max(floats)
        if result == 0.0 and any(v == 0x0000000000000000 for v in values):
            return 0x0000000000000000
        return bits(result)
    raise ValueError(op)


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument('vectors', nargs='?', type=Path,
                    default=Path(__file__).resolve().parents[1] / 'tests/float-vectors.json')
    data = json.loads(ap.parse_args().vectors.read_text(encoding='utf-8'))
    failures: list[str] = []

    ordered = [int(v['bits'], 16) for v in data['ascending_total_order']]
    if ordered != sorted(ordered, key=total_key):
        failures.append('ascending_total_order does not match IEEE totalOrder transform')

    for v in data['ordinary_equality']:
        left, right = f(int(v['left'], 16)), f(int(v['right'], 16))
        if (left == right) != v['equal']:
            failures.append(f"ordinary equality mismatch: {v}")

    for v in data['aggregate']:
        actual = aggregate(v['operation'], [int(x, 16) for x in v['inputs']])
        if actual != int(v['result'], 16):
            failures.append(f"aggregate mismatch: {v}; got {actual:016x}")

    for v in data['parquet_statistics']:
        vals = [int(x, 16) for x in v['values']]
        nans = [x for x in vals if is_nan_bits(x)]
        non = [x for x in vals if not is_nan_bits(x)]
        if len(nans) != v['nan_count']:
            failures.append(f"nan_count mismatch: {v}")
        if non:
            if min(non, key=total_key) != int(v['min_non_nan'], 16):
                failures.append(f"min_non_nan mismatch: {v}")
            if max(non, key=total_key) != int(v['max_non_nan'], 16):
                failures.append(f"max_non_nan mismatch: {v}")
        else:
            canon = [canonicalize_nan(x) for x in vals]
            if min(canon, key=total_key) != int(v['min_total'], 16):
                failures.append(f"all-NaN min mismatch: {v}")
            if max(canon, key=total_key) != int(v['max_total'], 16):
                failures.append(f"all-NaN max mismatch: {v}")

    if failures:
        print('FAIL')
        for item in failures:
            print(f'- {item}')
        return 1
    print(f"PASS: {len(ordered)} order values, {len(data['ordinary_equality'])} equality vectors, "
          f"{len(data['aggregate'])} aggregate vectors, {len(data['parquet_statistics'])} statistics vectors")
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
