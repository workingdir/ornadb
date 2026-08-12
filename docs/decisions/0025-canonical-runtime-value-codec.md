# ADR 0025: Canonical Runtime Values Use One Binary Codec

**Status:** Accepted

## Decision

Milestone 4 starts with one backend-independent value codec in a new
`orna-protocol` crate. Its public interface accepts and returns
`orna_core::value::RuntimeValue`. It does not define a second value model.
PostgreSQL, sockets, command-line parsing, JSON, presenters, and `sys.invoke`
stay outside this module.

Codec version 1 supports the complete runtime subset that is executable now:

* typed nulls for the supported standard scalar types and references;
* `BOOLEAN`, `INTEGER`, `BIGINT`, `FLOAT`, character data, and binary data; and
* typed object references with their target `TypeId` and `ObjectId`.

The stable standard-library `TypeId` values identify scalar values. A reference
uses its target object type identity. The codec does not accept a caller-made
inline type descriptor or infer a value type from a name.

## Version 1 bytes

Every encoded value has this exact byte sequence:

```text
offset  size  field
0       4     ASCII `ORV1`
4       1     value tag
5       16    TypeId bytes
21      4     unsigned payload length, big-endian
25      n     payload
```

The closed tags are:

```text
0x00  null standard scalar
0x01  null reference
0x02  boolean
0x03  signed 32-bit integer
0x04  signed 64-bit integer
0x05  finite IEEE-754 binary64 float
0x06  UTF-8 text
0x07  uninterpreted bytes
0x08  object reference
```

The scalar tags require their exact stable standard `TypeId`. A null scalar
requires one of the six standard types supported by `RuntimeValue`. A null
reference and a non-null reference accept a non-standard target `TypeId`; they
reject every stable standard scalar identity.

Null values have an empty payload. Boolean payload is exactly one byte, `0` or
`1`. Integers use two's-complement big-endian bytes. Float payload is the
big-endian IEEE-754 bit pattern. Encoding normalises negative zero to positive
zero. Decoding rejects negative zero and every non-finite float. Text is the
exact UTF-8 bytes of the Rust string. Binary data is unchanged. A reference
payload is the exact sixteen-byte `ObjectId`.

The maximum payload is 16 MiB, matching the current SERVER value budget. The
decoder checks the declared length before it reads a payload. It rejects a
bad marker, unknown tag, wrong stable type identity, wrong fixed payload
length, invalid Boolean, invalid UTF-8, non-canonical float, oversized payload,
truncation, and trailing bytes. It never returns a partial value.

## Module depth

Callers learn two operations: encode one runtime value and decode one complete
encoded value. Encoding returns a typed error for an oversized payload or a
reference whose target is any stable standard scalar identity. Decoding returns
a typed error for every rejected byte shape. Tag selection, stable type
mapping, size arithmetic, canonical numeric rules, and validation remain inside
the module. Tests use the same interface as production callers.

The value codec is not a socket frame. Later protocol frames carry these bytes
as bounded payloads and own negotiation, stream identity, flow control,
cancellation, and unknown-frame policy. The PostgreSQL backend protocol remains
private shell machinery and is not reused.

## Required proof

Tests must prove:

* exact golden bytes and round trips for every supported non-null value;
* exact golden bytes and round trips for every supported typed-null shape;
* positive and negative zero encode to the same bytes;
* each non-null scalar tag accepts only its matching supported standard
  `TypeId`;
* the null-scalar tag accepts exactly the six supported standard identities;
* the seven unsupported standard identities are rejected by every tag;
* both reference tags reject all thirteen stable standard identities;
* a reference retains both target and object identity;
* encoding rejects oversized text and bytes and a scalar identity used as a
  reference target;
* every one-byte Boolean value other than `0` and `1` is rejected;
* non-finite and negative-zero float bytes are rejected;
* malformed marker, tag, length, UTF-8, truncation, trailing bytes, and payload
  size fail closed; and
* decoding arbitrary bytes never panics.

Normal format, strict Clippy, rustdoc, diff, similarity, and workspace test
gates remain required.

## Implementation sequence

1. Accept this codec boundary and exact version-1 bytes.
2. Add `orna-protocol` with the deep encode/decode interface and direct tests.
3. Define the framed call, event, flow-control, and cancellation state machine
   in a separate decision and module.
4. Add an authenticated local-socket adapter only after the frame model is
   executable without a socket.

Each commit changes one to three files and keeps the repository buildable.

## Deferred surface

This decision does not add record, enum, opaque, collection, table, stream,
failure-value, or artifact encodings. Those need executable core value models
before the codec can encode them. It does not add socket I/O, TLS,
authentication frames, `CALL_RAW`, `sys.invoke`, CLI parsing, or presentation.
Every new value category and every incompatible representation requires a new
codec version. Version-1 tags and accepted value shapes remain closed.

## Precedence

This decision implements the first canonical-value part of milestone 4. It
narrows the broader value families in `spec/docs/27-wire-protocol.md` to the
complete runtime value set that the current trusted core can construct. It
does not mark the remaining protocol or richer-value checklist rows complete.
