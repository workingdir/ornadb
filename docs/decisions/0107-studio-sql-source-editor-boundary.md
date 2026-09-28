# Work ADR 0107: Studio SQL and Source Editor Boundary

**Status:** Accepted component boundary for GitHub issue #33

## Decision

The Studio source editor is a component that edits an in-memory `.orna`
document and presents language-service information supplied by `orna-lsp`.
It does not add a parser or infer editor diagnostics from source text. The
embedding Studio shell supplies the document URI and text, forwards component
text-change events to the language server, and supplies versioned diagnostics,
symbols, hover results, and navigation locations back to the component.

The editor component is renderer-neutral and owns only its current document
buffer, selection, and the latest language-service snapshot. A result is
visible only when its document version matches the buffer version. This keeps
stale diagnostics or navigation data from being presented after an edit.

The SQL/source editor covers Orna's SQL-like `.orna` source documents. It does
not define a separate SQL grammar or arbitrary query runner. It does not
execute source, apply or publish edits, activate revisions, browse history, or
perform hot reload. Those product operations remain outside issue #33 and are
governed by Work ADRs 0100 and 0106 where applicable.

This is a Studio component boundary, not a change to Orna 1.0.0. The frozen
reference defines stable diagnostic codes, concise titles, source spans when
available, causes, and actionable help where known (**ORNA-DIAG-001**,
`source/18-cli.md:74`). It also defines the effects of semantic LSP rename
(**ORNA-OBJECT-003**, `source/07-tables.md:444`); this editor does not perform
rename. No other Studio-specific behavior is inferred from these clauses.

## Component boundary

The component lives in `crates/orna-client/src/studio_source_editor.rs` and is
exposed by `orna-client`. The host integration contract is data-in/data-out:

* the host provides a document URI and `.orna` text;
* edits return a versioned complete-document change for the host to forward to
  `orna-lsp`;
* the host may install a language-service snapshot for that exact version;
* the component exposes diagnostics, document symbols, hover, definition, and
  references from that snapshot without recomputing them.

The component does not change the shared runtime event ABI. A host may connect
any suitable editor widget to the component's text-change API. This keeps the
editor isolated from catalogue-tree, Inspector, security/DBA, and source-reload
components.

## Reference search

The frozen reference was searched across the summary, `source/`, `grammar/`,
`tests/`, `examples/`, and `api/` for Studio, SQL/source editor, LSP, hover,
document symbols, navigation, interactive apply, and revision browser terms.
The only positive normative feature matches were ORNA-OBJECT-003 for semantic
rename and ORNA-DIAG-001 for diagnostic representation. The search returned
no Studio-specific editor or host-interaction contract. Work ADR 0100 records
its corresponding source-tooling search across the same corpus and explicitly
defers Studio editor workflows; this ADR supplies only the component-local
contract requested by issue #33 and adds no Orna language behavior.

## Relationship to existing decisions

Work ADR 0100 continues to defer Studio source apply and revision browsing.
Work ADR 0104 governs catalogue-tree search, ADR 0105 governs the security/DBA
page, and ADR 0106 governs source reload and hot revision. None of those
components shares this editor component's files or buffer state.
