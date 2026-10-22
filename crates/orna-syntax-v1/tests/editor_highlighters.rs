//! Runs every generated editor highlighter over one shared Orna sample and
//! checks the lexical class each host gives the same source spans.
//!
//! The expectations follow the Orna 1.1.0 lexical rules: comments
//! (ORNA-LEX-003/004), keywords and contextual names (ORNA-LEX-007,
//! "Contextual names and construction"), operators (ORNA-LEX-009),
//! interpolation (ORNA-LIT-004, "String escapes"), numerals (ORNA-LIT-001,
//! -005, -006, "Numeric and string token boundaries") and the grammar's
//! `identifier` (XID) production.
//!
//! Hosts: Emacs (`emacs --batch`), Vim (`vim` with +eval and +syntax; the
//! pattern check also runs on vim-minimal), the VS Code TextMate engine
//! (`node` with vscode-textmate and vscode-oniguruma under
//! `ORNA_EDITOR_TOOLS`), Sublime syntax (syntect, in process) and the
//! Tree-sitter CLI. A missing tool skips its host with the reason unless
//! `ORNA_REQUIRE_EDITORS=1`, which CI sets after installing every tool.

use std::{
    env, fs,
    io::ErrorKind,
    path::{Path, PathBuf},
    process::Command,
};

use syntect::parsing::{ParseState, Scope, ScopeStack, SyntaxDefinition, SyntaxSetBuilder};

const SAMPLE: &str = include_str!("fixtures/editor-highlight-sample.orna");

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Class {
    Keyword,
    Identifier,
    Number,
    Str,
    Comment,
    Operator,
    Punctuation,
    Unclassified,
}

impl Class {
    fn parse(name: &str) -> Self {
        match name {
            "keyword" => Self::Keyword,
            "identifier" => Self::Identifier,
            "number" => Self::Number,
            "string" => Self::Str,
            "comment" => Self::Comment,
            "operator" => Self::Operator,
            "punctuation" => Self::Punctuation,
            "none" => Self::Unclassified,
            other => panic!("unknown host class {other:?}"),
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum Expect {
    Is(Class),
    IsNot(Class),
}
use Class::*;
use Expect::{Is, IsNot};

/// `(context, target, expectation)`: `context` occurs exactly once in the
/// sample and contains `target`; every non-blank byte of `target` must have,
/// or must not have, the class.
const EXPECTATIONS: &[(&str, &str, Expect)] = &[
    // ORNA-LEX-003: a line comment runs to the end of its line.
    ("// line comment: let true 42", "let true 42", Is(Comment)),
    ("http://example.com/a", "example.com/a", Is(Comment)),
    // ORNA-LEX-004: `//` inside a block comment is comment text, and the
    // code after `*/` is code again.
    (
        "http://example.com/b with let */",
        "example.com/b with let",
        Is(Comment),
    ),
    ("*/ pub fn after_block", "pub", Is(Keyword)),
    ("after_block()", "after_block", Is(Identifier)),
    // ORNA-LEX-004: block comments nest, and a run of `*` closes them.
    ("inner */ still outer let */", "still outer let", Is(Comment)),
    ("pub fn after_nested", "after_nested", Is(Identifier)),
    ("/** doc comment **/", "doc comment", Is(Comment)),
    ("**/ pub fn after_stars", "after_stars", Is(Identifier)),
    // ORNA-LEX-007 keywords, identifiers and punctuation.
    ("pub fn greet", "pub", Is(Keyword)),
    ("pub fn greet", "fn", Is(Keyword)),
    ("fn greet", "greet", Is(Identifier)),
    ("greet(name: Str)", "(", Is(Punctuation)),
    ("greet(name: Str)", ":", Is(Punctuation)),
    ("): Str = \"hello", "=", Is(Operator)),
    // ORNA-LIT-004: interpolation is code inside a string.
    ("\"hello {name}", "hello", Is(Str)),
    ("hello {name} and", "{", Is(Punctuation)),
    ("hello {name} and", "name", Is(Identifier)),
    ("{name} and {label", "and", Is(Str)),
    ("{label(name)}", "label", Is(Identifier)),
    ("label(name)} done", "done", Is(Str)),
    // Interpolation uses the ordinary nested-delimiter rules: quotes and
    // braces inside it do not end the string or the interpolation.
    ("{f(\"x\")}", "f", Is(Identifier)),
    ("(\"x\")", "x", Is(Str)),
    ("\")} b {", "b", Is(Str)),
    ("{g({n: 1}.n)}", "g", Is(Identifier)),
    ("({n: 1}", "n", Is(Identifier)),
    ("{n: 1}", "1", Is(Number)),
    (".n)} c", "n", Is(Identifier)),
    (")} c\"", "c", Is(Str)),
    // "String escapes": an escape is string text, `\\` before `{` leaves
    // the brace to open an interpolation, and `\u{7b}` is not one.
    ("\"tab\\t", "\\t", Is(Str)),
    ("quote\\\" slash", "slash", Is(Str)),
    ("slash\\\\{name}", "name", Is(Identifier)),
    ("brace\\u{7b}", "\\u{7b}", Is(Str)),
    ("\\u{7b} end", "end", Is(Str)),
    // ORNA-LIT-001/005/006 numerals.
    ("[42,", "42", Is(Number)),
    ("1_000", "1_000", Is(Number)),
    ("0x1F", "0x1F", Is(Number)),
    ("0b1010", "0b1010", Is(Number)),
    ("3.1415", "3.1415", Is(Number)),
    ("2.5e-3", "2.5e-3", Is(Number)),
    ("6.02E+23f", "6.02E+23f", Is(Number)),
    (" 7f,", "7f", Is(Number)),
    ("7f, 2026-09-01,", "2026-09-01", Is(Number)),
    ("2026-09-01T14:30:00Z", "2026-09-01T14:30:00Z", Is(Number)),
    (
        "2026-09-01T14:30:00.123+02:00",
        "2026-09-01T14:30:00.123+02:00",
        Is(Number),
    ),
    ("12.34.decimal", "12.34", Is(Number)),
    ("12.34.decimal", "decimal", Is(Identifier)),
    // "Numeric and string token boundaries": a prefix needs a digit, an
    // underscore sits between digits, an exponent needs digits and an
    // instant fraction needs a digit.
    ("[x42,", "x42", Is(Identifier)),
    (" 0x,", "x", IsNot(Number)),
    ("1__2", "__2", IsNot(Number)),
    (" 1e,", "e", IsNot(Number)),
    ("00.Z]", "Z", IsNot(Number)),
    // "Contextual names and construction": a reserved word after `.` or as a
    // label is a name; `true`/`false` before `:` are case-arm literals.
    ("sys.ObjectKind.table", "table", Is(Identifier)),
    ("as: Message", "as", Is(Identifier)),
    ("{ type: 1", "type", Is(Identifier)),
    ("offset: 2", "offset", Is(Identifier)),
    ("{ true: 1", "true", Is(Keyword)),
    ("false: 0", "false", Is(Keyword)),
    ("0..self", "..", Is(Operator)),
    ("0..self", "self", Is(Keyword)),
    // `identifier`: XID_Start then XID_Continue, so a combining mark
    // continues the name.
    ("caf\u{e9} +", "caf\u{e9}", Is(Identifier)),
    ("cafe\u{301};", "cafe\u{301}", Is(Identifier)),
    // A keyword spelling inside a longer identifier is no keyword.
    ("formatting", "formatting", Is(Identifier)),
    ("letter", "letter", Is(Identifier)),
    ("iffy", "iffy", Is(Identifier)),
    // ORNA-LEX-009: longest-match operators.
    ("a ..= b", "..=", Is(Operator)),
    ("b ?? c", "??", Is(Operator)),
    ("c |? d", "|?", Is(Operator)),
    ("d || e", "||", Is(Operator)),
    ("e != f", "!=", Is(Operator)),
    ("f => g", "=>", Is(Operator)),
];

/// The class of every byte of the sample, line by line.
struct Classes(Vec<Vec<Class>>);

impl Classes {
    fn new() -> Self {
        Self(
            SAMPLE
                .lines()
                .map(|line| vec![Unclassified; line.len()])
                .collect(),
        )
    }

    fn set(&mut self, line: usize, start: usize, end: usize, class: Class) {
        let Some(bytes) = self.0.get_mut(line) else {
            return;
        };
        let end = end.min(bytes.len());
        if start < end {
            bytes[start..end].fill(class);
        }
    }

    /// Reads "LINE START END CLASS" rows printed by a host driver.
    fn from_rows(rows: &str) -> Self {
        let mut classes = Self::new();
        for row in rows.lines().filter(|row| !row.trim().is_empty()) {
            let fields = row.split_whitespace().collect::<Vec<_>>();
            let [line, start, end, class] = fields.as_slice() else {
                panic!("malformed host row {row:?}");
            };
            classes.set(
                line.parse().expect("row line"),
                start.parse().expect("row start"),
                end.parse().expect("row end"),
                Class::parse(class),
            );
        }
        classes
    }

    fn at(&self, offset: usize) -> Class {
        let mut line_start = 0;
        for (index, line) in SAMPLE.lines().enumerate() {
            if offset < line_start + line.len() {
                return self.0[index][offset - line_start];
            }
            line_start += line.len() + 1;
        }
        panic!("offset {offset} is past the sample");
    }
}

/// Checks every expectation and reports each disagreement for `host`.
fn assert_expectations(host: &str, classes: &Classes) {
    let mut failures = Vec::new();
    for &(context, target, expect) in EXPECTATIONS {
        let context_at = SAMPLE.find(context).expect("expectation context");
        let target_at = context_at + context.find(target).expect("target inside context");
        for (index, byte) in target.bytes().enumerate() {
            if byte.is_ascii_whitespace() {
                continue;
            }
            let actual = classes.at(target_at + index);
            let holds = match expect {
                Is(class) => actual == class,
                IsNot(class) => actual != class,
            };
            if !holds {
                failures.push(format!(
                    "{host}: {target:?} in {context:?}: byte {index} is {actual:?}, expected {expect:?}"
                ));
                break;
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

fn manifest_dir() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

fn repository_root() -> &'static Path {
    manifest_dir()
        .ancestors()
        .nth(2)
        .expect("orna-syntax-v1 is under crates/")
}

fn sample_path() -> PathBuf {
    manifest_dir().join("tests/fixtures/editor-highlight-sample.orna")
}

fn host_driver(name: &str) -> PathBuf {
    manifest_dir().join("tests/editor_hosts").join(name)
}

fn editors_required() -> bool {
    env::var("ORNA_REQUIRE_EDITORS").as_deref() == Ok("1")
}

/// Skips a host whose tool is missing, or fails when CI requires it.
fn skip(host: &str, reason: &str) {
    assert!(!editors_required(), "{host}: {reason}");
    eprintln!("SKIP: {host}: {reason}");
}

fn tool(variable: &str, default: &str) -> PathBuf {
    env::var_os(variable)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(default))
}

/// Runs a host command; `None` means the program is not installed.
fn run(command: &mut Command) -> Option<std::process::Output> {
    match command.output() {
        Ok(output) => Some(output),
        Err(error) if error.kind() == ErrorKind::NotFound => None,
        Err(error) => panic!("cannot start {command:?}: {error}"),
    }
}

fn successful_stdout(host: &str, output: std::process::Output) -> String {
    assert!(
        output.status.success(),
        "{host} failed ({}):\n{}\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("host output is UTF-8")
}

fn scratch_directory(name: &str) -> PathBuf {
    let directory = env::temp_dir().join(format!(
        "orna-editor-highlighters-{name}-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&directory);
    fs::create_dir_all(&directory).expect("create scratch directory");
    directory
}

/// Scope-name prefixes in the order a token's innermost scope is matched.
const SCOPE_CLASSES: &[(&str, Class)] = &[
    ("comment.", Comment),
    ("constant.character.escape.", Str),
    ("string.", Str),
    ("keyword.control.", Keyword),
    ("keyword.operator.", Operator),
    ("variable.", Identifier),
    ("constant.numeric.", Number),
    ("punctuation.", Punctuation),
];

fn scope_class(scopes: &[String]) -> Class {
    if scopes.iter().any(|scope| scope.starts_with("comment.")) {
        return Comment;
    }
    scopes
        .iter()
        .rev()
        .find_map(|scope| {
            SCOPE_CLASSES
                .iter()
                .find(|(prefix, _)| scope.starts_with(prefix))
                .map(|&(_, class)| class)
        })
        .unwrap_or(Unclassified)
}

#[test]
fn expectations_name_unique_spans_of_the_sample() {
    for &(context, target, _) in EXPECTATIONS {
        assert_eq!(
            SAMPLE.matches(context).count(),
            1,
            "context {context:?} must occur exactly once"
        );
        assert!(context.contains(target), "{target:?} is not in {context:?}");
    }
    assert_eq!(
        fs::read_to_string(sample_path()).expect("sample reads"),
        SAMPLE
    );
}

#[test]
fn emacs_orna_mode_classifies_the_shared_sample() {
    let emacs = tool("ORNA_TEST_EMACS", "emacs");
    let Some(output) = run(Command::new(&emacs)
        .args(["--batch", "-Q", "-l"])
        .arg(repository_root().join("editors/emacs/orna-eglot.el"))
        .arg("-l")
        .arg(host_driver("emacs-faces.el"))
        .arg(sample_path()))
    else {
        return skip("Emacs", "emacs is not installed; install emacs-nox");
    };
    let rows = successful_stdout("Emacs", output);
    assert_expectations("Emacs", &Classes::from_rows(&rows));
}

/// True when `vim --version` lists both features.
fn vim_has_eval_and_syntax(vim: &Path) -> Option<bool> {
    let output = run(Command::new(vim).arg("--version"))?;
    let version = String::from_utf8_lossy(&output.stdout);
    Some(version.contains("+eval") && version.contains("+syntax"))
}

#[test]
fn vim_syntax_classifies_the_shared_sample() {
    let vim = tool("ORNA_TEST_VIM", "vim");
    match vim_has_eval_and_syntax(&vim) {
        None => return skip("Vim", "vim is not installed; install the full vim package"),
        Some(false) => {
            return skip(
                "Vim",
                "vim lacks +eval or +syntax (vim-minimal); install the full vim package",
            );
        }
        Some(true) => {}
    }
    let scratch = scratch_directory("vim");
    let output_path = scratch.join("classes.txt");
    let output = run(Command::new(&vim)
        .args(["-Es", "-u", "NONE", "-i", "NONE", "-N", "-S"])
        .arg(host_driver("vim-syntax.vim"))
        .env("ORNA_SAMPLE", sample_path())
        .env(
            "ORNA_VIM_SYNTAX",
            repository_root().join("editors/vim/syntax/orna.vim"),
        )
        .env("ORNA_OUTPUT", &output_path))
    .expect("vim ran a moment ago");
    successful_stdout("Vim", output);
    let rows = fs::read_to_string(&output_path).expect("Vim wrote its classes");
    let _ = fs::remove_dir_all(&scratch);
    assert_expectations("Vim", &Classes::from_rows(&rows));
}

/// The generated `syntax match` pattern of `group`, without its delimiters.
fn vim_match_pattern(syntax: &str, group: &str) -> (char, String) {
    let line = syntax
        .lines()
        .find_map(|line| line.strip_prefix(&format!("syntax match {group} ")))
        .unwrap_or_else(|| panic!("no syntax match for {group}"));
    let delimiter = line.chars().next().expect("a pattern delimiter");
    let body = &line[delimiter.len_utf8()..];
    let end = body.rfind(delimiter).expect("a closing delimiter");
    (delimiter, body[..end].to_owned())
}

/// Vim's regex engine on the generated number and identifier patterns. This
/// needs neither +eval nor +syntax, so it also runs on vim-minimal.
#[test]
fn vim_patterns_match_whole_numerals_and_identifiers() {
    let vi = tool("ORNA_TEST_VI", "vi");
    let syntax = fs::read_to_string(repository_root().join("editors/vim/syntax/orna.vim"))
        .expect("the Vim syntax reads");
    let cases = [
        ("ornaNumber", "42", "[42]"),
        ("ornaNumber", "1_000", "[1_000]"),
        ("ornaNumber", "0x1F", "[0x1F]"),
        ("ornaNumber", "0b1010", "[0b1010]"),
        ("ornaNumber", "3.1415", "[3.1415]"),
        ("ornaNumber", "2.5e-3", "[2.5e-3]"),
        ("ornaNumber", "6.02E+23f", "[6.02E+23f]"),
        ("ornaNumber", "7f", "[7f]"),
        ("ornaNumber", "2026-09-01", "[2026-09-01]"),
        ("ornaNumber", "2026-09-01T14:30:00Z", "[2026-09-01T14:30:00Z]"),
        (
            "ornaNumber",
            "2026-09-01T14:30:00.123+02:00",
            "[2026-09-01T14:30:00.123+02:00]",
        ),
        ("ornaNumber", "x42", "x42"),
        ("ornaNumber", "0x", "0x"),
        ("ornaNumber", "1__2", "1__2"),
        ("ornaNumber", "1e", "1e"),
        ("ornaIdentifier", "caf\u{e9}", "[caf\u{e9}]"),
        ("ornaIdentifier", "cafe\u{301}", "[cafe\u{301}]"),
        ("ornaIdentifier", "_x42", "[_x42]"),
    ];
    let scratch = scratch_directory("vi");
    for group in ["ornaNumber", "ornaIdentifier"] {
        let (delimiter, pattern) = vim_match_pattern(&syntax, group);
        let inputs = cases
            .iter()
            .filter(|(case_group, _, _)| *case_group == group)
            .collect::<Vec<_>>();
        let input_path = scratch.join(format!("{group}.txt"));
        let output_path = scratch.join(format!("{group}.out"));
        fs::write(
            &input_path,
            inputs
                .iter()
                .map(|(_, input, _)| format!("{input}\n"))
                .collect::<String>(),
        )
        .expect("write the Vim input");
        let substitute = format!("silent! %s{delimiter}{pattern}{delimiter}[&]{delimiter}g");
        let Some(output) = run(Command::new(&vi)
            .args(["-e", "-s", "-N", "-u", "NONE", "-i", "NONE", "-c"])
            .arg(&substitute)
            .arg("-c")
            .arg(format!("write! {}", output_path.display()))
            .args(["-c", "quit!"])
            .arg(&input_path))
        else {
            let _ = fs::remove_dir_all(&scratch);
            return skip("Vim patterns", "vi is not installed; install vim or vim-minimal");
        };
        assert!(output.status.success(), "vi failed: {output:?}");
        let actual = fs::read_to_string(&output_path).expect("vi wrote its output");
        let expected = inputs
            .iter()
            .map(|(_, _, expected)| format!("{expected}\n"))
            .collect::<String>();
        assert_eq!(actual, expected, "{group} pattern {pattern}");
    }
    let _ = fs::remove_dir_all(&scratch);
}

#[test]
fn textmate_grammar_classifies_the_shared_sample() {
    let Some(tools) = env::var_os("ORNA_EDITOR_TOOLS") else {
        return skip(
            "TextMate",
            "set ORNA_EDITOR_TOOLS to a directory where `npm install vscode-textmate@9.3.2 vscode-oniguruma@2.0.1` ran",
        );
    };
    let node = tool("ORNA_TEST_NODE", "node");
    let Some(output) = run(Command::new(&node)
        .arg(host_driver("textmate.mjs"))
        .arg(repository_root().join("editors/vscode/syntaxes/orna.tmLanguage.json"))
        .arg(sample_path())
        .env("ORNA_EDITOR_TOOLS", tools))
    else {
        return skip("TextMate", "node is not installed");
    };
    let rows = successful_stdout("TextMate", output);
    assert_expectations("TextMate", &Classes::from_rows(&rows));
}

#[test]
fn sublime_syntax_loads_and_classifies_the_shared_sample() {
    let source = fs::read_to_string(repository_root().join("editors/sublime/Orna.sublime-syntax"))
        .expect("the Sublime syntax reads");
    let definition =
        SyntaxDefinition::load_from_str(&source, true, None).expect("the Sublime syntax loads");
    let mut builder = SyntaxSetBuilder::new();
    builder.add(definition);
    let set = builder.build();
    let syntax = set
        .find_syntax_by_scope(Scope::new("source.orna").expect("a scope"))
        .expect("the Orna syntax is registered");
    let mut state = ParseState::new(syntax);
    let mut stack = ScopeStack::new();
    let mut classes = Classes::new();
    for (index, line) in SAMPLE.lines().enumerate() {
        let operations = state
            .parse_line(&format!("{line}\n"), &set)
            .expect("the sample line parses");
        let mut position = 0;
        for (offset, operation) in operations {
            let scopes = stack
                .as_slice()
                .iter()
                .copied()
                .map(Scope::build_string)
                .collect::<Vec<_>>();
            classes.set(index, position, offset, scope_class(&scopes));
            stack.apply(&operation).expect("the scope operation applies");
            position = offset;
        }
        let scopes = stack
            .as_slice()
            .iter()
            .copied()
            .map(Scope::build_string)
            .collect::<Vec<_>>();
        classes.set(index, position, line.len(), scope_class(&scopes));
    }
    assert_expectations("Sublime", &classes);
}

/// Class of one `highlights.scm` capture name.
fn capture_class(name: &str) -> Class {
    match name.split('.').next().unwrap_or(name) {
        "keyword" => Keyword,
        "variable" => Identifier,
        "number" => Number,
        "string" => Str,
        "comment" => Comment,
        "operator" => Operator,
        "punctuation" => Punctuation,
        other => panic!("unexpected Tree-sitter capture {other:?}"),
    }
}

/// Reads `(row, column)` from the text after `label`.
fn query_point(line: &str, label: &str) -> (usize, usize) {
    let rest = &line[line.find(label).expect("a capture point") + label.len()..];
    let inner = &rest[..rest.find(')').expect("a closing parenthesis")];
    let (row, column) = inner.split_once(',').expect("a row and a column");
    (
        row.trim().parse().expect("a row"),
        column.trim().parse().expect("a column"),
    )
}

#[test]
fn tree_sitter_grammar_and_highlights_classify_the_shared_sample() {
    let tree_sitter = tool("ORNA_TEST_TREE_SITTER", "tree-sitter");
    let scratch = scratch_directory("tree-sitter");
    let grammar = repository_root().join("editors/tree-sitter-orna");
    fs::create_dir_all(scratch.join("queries")).expect("create the query directory");
    for file in [
        "grammar.js",
        "package.json",
        "tree-sitter.json",
        "queries/highlights.scm",
    ] {
        fs::copy(grammar.join(file), scratch.join(file)).expect("copy the grammar package");
    }
    let Some(generated) = run(Command::new(&tree_sitter)
        .arg("generate")
        .current_dir(&scratch))
    else {
        let _ = fs::remove_dir_all(&scratch);
        return skip(
            "Tree-sitter",
            "tree-sitter is not installed; npm install tree-sitter-cli@0.26.5",
        );
    };
    successful_stdout("tree-sitter generate", generated);
    let output = run(Command::new(&tree_sitter)
        .arg("query")
        .arg(scratch.join("queries/highlights.scm"))
        .arg(sample_path())
        .current_dir(&scratch)
        .env("TREE_SITTER_LIBDIR", scratch.join("lib")))
    .expect("tree-sitter ran a moment ago");
    let captures = successful_stdout("tree-sitter query", output);
    let _ = fs::remove_dir_all(&scratch);

    let mut spans = captures
        .lines()
        .filter_map(|line| {
            let rest = line.trim().strip_prefix("capture:")?;
            let name = rest
                .split_once(" - ")
                .map_or(rest, |(_, name)| name)
                .split(',')
                .next()
                .expect("a capture name")
                .trim();
            Some((
                query_point(line, "start: ("),
                query_point(line, "end: ("),
                capture_class(name),
            ))
        })
        .collect::<Vec<_>>();
    assert!(!spans.is_empty(), "tree-sitter reported no captures:\n{captures}");
    // The innermost capture decides, as in highlighting: apply wide spans
    // first so the nodes nested in them overwrite their bytes.
    let line_lengths = SAMPLE.lines().map(str::len).collect::<Vec<_>>();
    let width = |((start_row, start_column), (end_row, end_column)): ((usize, usize), (usize, usize))| {
        let mut bytes = 0;
        for row in start_row..=end_row {
            let from = if row == start_row { start_column } else { 0 };
            let to = if row == end_row { end_column } else { line_lengths[row] + 1 };
            bytes += to.saturating_sub(from);
        }
        bytes
    };
    spans.sort_by_key(|&(start, end, _)| std::cmp::Reverse(width((start, end))));
    let mut classes = Classes::new();
    for ((start_row, start_column), (end_row, end_column), class) in spans {
        for row in start_row..=end_row {
            let from = if row == start_row { start_column } else { 0 };
            let to = if row == end_row { end_column } else { usize::MAX };
            classes.set(row, from, to, class);
        }
    }
    assert_expectations("Tree-sitter", &classes);
}
