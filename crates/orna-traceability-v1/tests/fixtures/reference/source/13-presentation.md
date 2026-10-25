# 13. Inspection, display, presentation and codecs {#presentation}

## Four distinct concepts

Orna distinguishes:

1. **Inspect** - structural developer representation.
2. **Display** - friendly human text.
3. **Present** - rich typed rendering tree.
4. **Codec** - stable typed serialization/deserialization.

**ORNA-PRES-001** Implementations MUST NOT treat terminal presentation as canonical persistence encoding.

## Inspect

Every value has an automatically derived structural fallback.

```text
Money<GBP> {
    coefficient: 1234,
    scale: 2,
}
```

**ORNA-PRES-002** Inspect MUST expose type and structure, may truncate large collections, and MUST redact secret values.

## Display

Display is friendly text:

```text
£12.34
2h 14m
Alice Smith
```

**ORNA-PRES-003** Display MAY be lossy and contextual.

**ORNA-PRES-004** Changing Display MUST NOT change equality, hashing, storage, Git object IDs, query results or codec output.

## Dynamic display formatters

Formatter values may depend on locale, time zone, precision, width and unit preferences.

```text
std.time.duration.compact.format(duration) // "2h 14m"
std.time.duration.clock.format(duration)   // "02:14:00"
std.time.duration.words.format(duration)   // "2 hours, 14 minutes"
std.time.duration.iso.format(duration)     // "PT2H14M"
```

A REPL session may override defaults:

```text
:display Duration std.time.duration.clock
:display Duration default
```

A page may override presentation locally:

```orna
Table(rows, columns: {
    duration: Column(display: std.time.duration.clock),
})
```

**ORNA-PRES-005** Session display overrides MUST be ephemeral unless the user explicitly saves presentation settings.

## User-defined Inspect, Display and Present

Version 1.0 defines the same minimal protocol mechanism used by ordinary generic code:

```orna
pub type FriendlyDuration = Duration {
    impl Display {
        fn display(self, context: DisplayContext): Str =
            std.time.duration.compact(self, context);
    }
}

pub table Contact(id: Uuid) {
    name: Str,
    emails: [Str],

    impl Present {
        fn present(self, context: PresentContext): PresentTree =
            std.ui.Details([
                ("Name", self.name),
                ("Emails", self.emails),
            ]);
    }
}
```

**ORNA-PRES-006** Every value has a host-derived, cycle-safe and bounded Inspect fallback.

**ORNA-PRES-007** Display returns human-oriented `Str`; Present returns the core typed presentation tree.

**ORNA-PRES-008** Display/Present implementations are read-only, deterministic relative to their immutable context and subject to time/instruction/output budgets. Database writes, connectors, network, filesystem, process execution, secret reveal and nondeterministic time/randomness are unavailable.

**ORNA-PRES-009** A presenter failure falls back to Inspect with a diagnostic.

## Present

Present returns a typed tree that may represent tables, objects, source, diffs, charts and custom widgets.

Default selection order:

```text
explicit presenter
-> session/page presentation override
-> type Present
-> type Display
-> derived Inspect
```

**ORNA-PRES-010** A renderer that does not recognize a rich node MUST fall back to an Inspect-compatible representation rather than fail to show the value.

## Codecs

Codecs own their operations:

```text
std.encoding.json.encode(value)
std.encoding.json.decode(text, as: Contact)

std.encoding.orna.encode(value)
std.encoding.orna.decode(text, as: Contact)
```

**ORNA-CODEC-001** A codec MUST provide typed `encode(value)` and `decode(input, as: T)` operations.

**ORNA-CODEC-002** Decode MUST produce `T` on success or fail with a `DecodeError` preserving format location/path information. It MUST NOT wrap the successful value in `Result`.

**ORNA-CODEC-003** Canonical Orna encoding MUST be deterministic, full-precision, unambiguous and round-trippable for supported values.

## Row-body encoding versus standalone encoding

A VFS row document omits its key because the key is selected by its row directory. Legacy loose-row interchange uses its recorded path codec. Neither projection is a second format-3 stored row copy.

Standalone encoding of the logical row includes the key:

```orna
{
    id: "alice-smith",
    name: "Alice Smith",
    emails: ["alice@example.com"],
}
```

**ORNA-CODEC-004** Generic value encoding of a table row MUST include all logical fields, including primary-key fields.

**ORNA-CODEC-005** Loading a row document MUST combine its admitted table schema, explicit/path key and decoded stored fields under the selected interchange/VFS context; arbitrary host paths cannot supply authority. Format-3 persistence encodes the key and stored fields directly through ORP-1.

**ORNA-CODEC-006** Exact checked-out source text MAY differ from canonical re-encoding because comments and formatting may be preserved. Exact source is obtained through `sys.File`, not by encoding the semantic value.

## Blob values {#blob-presentation}

The default structural [Blob inspector](#blob-values) presents safe length/content metadata without materialising bytes. Rich media viewing or downloading is explicit and uses a snapshot-bound, authorised Blob reader. Declared media type and optional suffix are Blob annotations; byte identity is separate. A filename is a VFS projection, not a content identity. Inspecting arbitrary source, HTML or SVG must not execute it. No devtools-specific Asset table is required by a renderer.


## Codec selection in format 3

The legacy portable text/OVB-1 descriptions are compatibility encodings, not the new annotated-Blob row representation. Format-3 values use [OVB-2/ROV-3/SOV-3](#formats). New Blob-bearing human row documents use small references through VFS-1 or explicit complete snapshot export; they never expand media as Base64. Full portable byte export is explicit and bounded/streamed.
