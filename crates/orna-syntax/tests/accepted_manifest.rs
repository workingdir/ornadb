use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use orna_syntax::{Parse, parse};

/// The accepted corpus is the one source of truth for this focused syntax gate.
/// The editor tooling check owns the separate accepted/deferred parity check;
/// this test parses only cases named by that manifest and never reads canonical
/// proposal examples.
const ACCEPTED_MANIFEST: &str =
    include_str!("../../../editors/tree-sitter-orna/test/accepted-corpus.txt");
const CORPUS_DELIMITER: &str = "====================";

// These accepted editor cases are intentionally retained as lossless source
// plus diagnostics by the public parser, which does not expose declarations
// for their query/body shapes. Keep the exceptions named and bounded: any new
// diagnostic-bearing non-ERROR case must be reviewed here before parity is
// skipped.
const EDITOR_ONLY_DIAGNOSTIC_CASES: &[&str] = &[
    "expression literals and qualified names",
    "server function with select body",
    "server function with rows return and select distinct",
    "server function with insert body",
    "server function with update body",
    "server function with delete body",
    "create enum type",
    "create object type with field modifiers",
    "unicode unquoted identifiers",
];

#[derive(Debug)]
struct CorpusCase {
    source: String,
    expected_rejection: bool,
    expected_tree: String,
    path: PathBuf,
}

/// The public parser exposes typed declaration collections, but its lossless
/// `SyntaxTree` boundary intentionally exposes only source text. Keep the
/// parity check at that public boundary: compare editor tree declaration
/// families and counts, not private Rowan node kinds or the full EBNF shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum PublicDeclarationFamily {
    Schemas,
    ObjectTypes,
    EnumTypes,
    ValueTypes,
    TypeExports,
    FieldRenames,
    ServerFunctions,
    ClientFunctions,
}

fn expected_node_names(expected_tree: &str) -> Vec<&str> {
    let mut lines = expected_tree.lines();
    assert_eq!(
        lines.next(),
        Some("(source_file"),
        "accepted corpus expected tree must have a source_file root"
    );

    lines
        .filter_map(|line| {
            let rest = line.trim_start().strip_prefix('(')?;
            let end = rest
                .find(|character: char| character.is_whitespace() || character == ')')
                .unwrap_or(rest.len());
            Some(&rest[..end])
        })
        .collect()
}

fn expected_public_declaration_families(
    expected_tree: &str,
) -> BTreeMap<PublicDeclarationFamily, usize> {
    let mut families = BTreeMap::new();
    for node_name in expected_node_names(expected_tree) {
        let family = match node_name {
            "create_schema_statement" => Some(PublicDeclarationFamily::Schemas),
            "create_object_type_statement" => Some(PublicDeclarationFamily::ObjectTypes),
            "create_enum_type_statement" => Some(PublicDeclarationFamily::EnumTypes),
            // The public parser splits VALUE declarations by their retained
            // kind (record, primitive, or opaque), while the editor grammar
            // uses one create_value_type_statement node.
            "create_value_type_statement" => Some(PublicDeclarationFamily::ValueTypes),
            "export_type_statement" => Some(PublicDeclarationFamily::TypeExports),
            "create_server_function_statement" => Some(PublicDeclarationFamily::ServerFunctions),
            "create_client_function_statement" | "create_external_client_function_statement" => {
                Some(PublicDeclarationFamily::ClientFunctions)
            }
            // The accepted manifest currently includes only the field-rename
            // ALTER form. Other editor ALTER nodes have no public Parse
            // accessor, so they are intentionally outside this bounded check.
            "alter_statement" if expected_tree.contains("old_name:") => {
                Some(PublicDeclarationFamily::FieldRenames)
            }
            _ => None,
        };
        if let Some(family) = family {
            *families.entry(family).or_insert(0) += 1;
        }
    }
    families
}

fn public_declaration_families(parsed: &Parse) -> BTreeMap<PublicDeclarationFamily, usize> {
    let mut families = BTreeMap::new();
    let mut add = |family, count| {
        if count != 0 {
            families.insert(family, count);
        }
    };
    add(PublicDeclarationFamily::Schemas, parsed.schemas().len());
    add(
        PublicDeclarationFamily::ObjectTypes,
        parsed.object_types().len(),
    );
    add(
        PublicDeclarationFamily::EnumTypes,
        parsed.enum_types().len(),
    );
    add(
        PublicDeclarationFamily::ValueTypes,
        parsed.record_value_types().len()
            + parsed.primitive_value_types().len()
            + parsed.opaque_value_types().len(),
    );
    add(
        PublicDeclarationFamily::TypeExports,
        parsed.type_exports().len(),
    );
    add(
        PublicDeclarationFamily::FieldRenames,
        parsed.field_renames().len(),
    );
    add(
        PublicDeclarationFamily::ServerFunctions,
        parsed.server_functions().len(),
    );
    add(
        PublicDeclarationFamily::ClientFunctions,
        parsed.client_functions().len(),
    );
    families
}

fn accepted_case_names() -> Vec<String> {
    let names = ACCEPTED_MANIFEST
        .lines()
        .enumerate()
        .map(|(line_number, line)| {
            assert!(
                !line.is_empty() && line == line.trim(),
                "malformed accepted corpus manifest entry at line {}: {line:?}",
                line_number + 1
            );
            line.to_owned()
        })
        .collect::<Vec<_>>();

    assert!(
        !names.is_empty(),
        "accepted corpus manifest must enumerate at least one case"
    );
    let mut unique_names = names.clone();
    unique_names.sort();
    unique_names.dedup();
    assert_eq!(
        unique_names.len(),
        names.len(),
        "accepted corpus manifest contains duplicate case names"
    );
    names
}

fn corpus_directory() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../editors/tree-sitter-orna/test/corpus")
}

fn lines_with_offsets(source: &str) -> Vec<(usize, usize)> {
    let mut lines = Vec::new();
    let mut start = 0;
    for line in source.split_inclusive('\n') {
        let end = start + line.len();
        lines.push((start, end));
        start = end;
    }
    if start < source.len() {
        lines.push((start, source.len()));
    }
    lines
}

fn line_body(source: &str, range: (usize, usize)) -> &str {
    let line = &source[range.0..range.1];
    let line = line.strip_suffix('\n').unwrap_or(line);
    line.strip_suffix('\r').unwrap_or(line)
}

fn parse_corpus_file(path: &Path, contents: &str) -> Vec<(String, CorpusCase)> {
    let lines = lines_with_offsets(contents);
    let mut cases = Vec::new();
    let mut cursor = 0;

    while cursor < lines.len() {
        let body = line_body(contents, lines[cursor]);
        if body.is_empty() {
            cursor += 1;
            continue;
        }
        assert_eq!(
            body,
            CORPUS_DELIMITER,
            "expected corpus case delimiter in {} at line {}",
            path.display(),
            cursor + 1
        );
        assert!(
            cursor + 2 < lines.len(),
            "truncated corpus case header in {} at line {}",
            path.display(),
            cursor + 1
        );
        let name = line_body(contents, lines[cursor + 1]);
        assert!(
            !name.is_empty() && name == name.trim(),
            "malformed corpus case name in {} at line {}: {name:?}",
            path.display(),
            cursor + 2
        );
        assert_eq!(
            line_body(contents, lines[cursor + 2]),
            CORPUS_DELIMITER,
            "malformed corpus case header in {} at line {}",
            path.display(),
            cursor + 3
        );

        let mut source_line = cursor + 3;
        // The blank line after the header is corpus framing, not source text.
        if source_line < lines.len() && line_body(contents, lines[source_line]).is_empty() {
            source_line += 1;
        }
        let separator_line = (source_line..lines.len())
            .find(|&index| line_body(contents, lines[index]) == "---")
            .unwrap_or_else(|| {
                panic!(
                    "corpus case {name:?} in {} has no `---` source separator",
                    path.display()
                )
            });
        let mut source_end = lines[separator_line].0;
        // The blank line before `---` is also corpus framing.
        if separator_line > source_line && line_body(contents, lines[separator_line - 1]).is_empty()
        {
            source_end = lines[separator_line - 1].0;
        }
        let source = contents[lines[source_line].0..source_end].to_owned();

        let expected_tree_start = lines[separator_line].1;
        let next_case = ((separator_line + 1)..lines.len())
            .find(|&index| line_body(contents, lines[index]) == CORPUS_DELIMITER);
        let expected_tree_end = next_case.map_or(contents.len(), |index| lines[index].0);
        let expected_tree = contents[expected_tree_start..expected_tree_end].trim();
        assert!(
            !expected_tree.is_empty(),
            "corpus case {name:?} in {} has no expected tree",
            path.display()
        );

        let expected_rejection = expected_tree.contains("(ERROR");
        cases.push((
            name.to_owned(),
            CorpusCase {
                source,
                expected_rejection,
                expected_tree: expected_tree.to_owned(),
                path: path.to_owned(),
            },
        ));
        cursor = next_case.unwrap_or(lines.len());
    }

    cases
}

fn corpus_cases() -> BTreeMap<String, CorpusCase> {
    let mut paths = fs::read_dir(corpus_directory())
        .unwrap_or_else(|error| panic!("read accepted corpus directory: {error}"))
        .map(|entry| {
            entry
                .unwrap_or_else(|error| panic!("read accepted corpus directory entry: {error}"))
                .path()
        })
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension.to_str() == Some("txt"))
        })
        .collect::<Vec<_>>();
    paths.sort();

    let mut cases = BTreeMap::new();
    for path in paths {
        let contents = fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("read corpus file {}: {error}", path.display()));
        for (name, case) in parse_corpus_file(&path, &contents) {
            assert!(
                cases.insert(name.clone(), case).is_none(),
                "duplicate corpus case name {name:?}"
            );
        }
    }
    assert!(!cases.is_empty(), "accepted corpus contains no cases");
    cases
}

#[test]
fn accepted_manifest_sources_match_public_parser_subset() {
    let names = accepted_case_names();
    let cases = corpus_cases();

    for name in names {
        let case = cases.get(&name).unwrap_or_else(|| {
            panic!("accepted corpus manifest case {name:?} has no source fixture")
        });
        let parsed = parse(&case.source);
        assert_eq!(
            parsed.syntax().text(),
            case.source,
            "accepted corpus fixture {name:?} from {} was not preserved losslessly",
            case.path.display()
        );

        for diagnostic in parsed.diagnostics() {
            assert!(
                diagnostic.span.start <= diagnostic.span.end
                    && case
                        .source
                        .get(diagnostic.span.start..diagnostic.span.end)
                        .is_some(),
                "accepted corpus fixture {name:?} from {} has an invalid diagnostic span {:?}",
                case.path.display(),
                diagnostic.span
            );
        }

        // The editor corpus records syntax acceptance. Some accepted editor
        // cases intentionally use query or type forms that the public Rust
        // parser preserves losslessly but reports as semantic diagnostics.
        // Keep intentional `(ERROR ...)` cases diagnostic-bearing without
        // treating the editor grammar corpus as a Rust execution contract.
        if !case.expected_rejection && parsed.diagnostics().is_empty() {
            assert_eq!(
                public_declaration_families(&parsed),
                expected_public_declaration_families(&case.expected_tree),
                "accepted corpus fixture {name:?} from {} disagrees on the bounded public declaration shape",
                case.path.display()
            );
        } else if !case.expected_rejection {
            assert!(
                EDITOR_ONLY_DIAGNOSTIC_CASES.contains(&name.as_str()),
                "accepted corpus fixture {name:?} from {} has unexpected diagnostics; name any intentional editor-only exception explicitly",
                case.path.display()
            );
        }
        if case.expected_rejection {
            assert!(
                !parsed.diagnostics().is_empty(),
                "accepted corpus fixture {name:?} from {} has an `(ERROR ...)` tree but no parser diagnostics",
                case.path.display()
            );
        }
    }
}
