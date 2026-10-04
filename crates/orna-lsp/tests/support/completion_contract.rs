use std::collections::BTreeSet;

use orna_syntax_v1::Keyword;
use serde_json::Value;

pub fn expected_keywords() -> BTreeSet<String> {
    Keyword::ALL
        .iter()
        .map(|keyword| keyword.spelling().to_owned())
        .collect()
}

pub fn assert_lsp_completion_contract(completion: &Value, editor: &str) {
    let items = completion
        .as_array()
        .or_else(|| completion["items"].as_array())
        .unwrap_or_else(|| panic!("{editor} completion result is neither an array nor a list"));
    let actual_keywords = items
        .iter()
        .filter(|item| item["kind"] == 14)
        .map(|item| item["label"].as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    let actual_keyword_count = actual_keywords.len();
    let actual_keywords = actual_keywords.into_iter().collect::<BTreeSet<_>>();
    assert_eq!(
        actual_keywords,
        expected_keywords(),
        "{editor} ORNA-LEX-007 completion inventory"
    );
    assert_eq!(
        actual_keyword_count,
        expected_keywords().len(),
        "{editor} ORNA-LEX-007 completion inventory has duplicate keywords"
    );
    assert!(!completion.to_string().contains("\"CREATE\""));
    assert!(!completion.to_string().contains("\"SELECT\""));

    let add = items
        .iter()
        .find(|item| item["label"] == "add")
        .unwrap_or_else(|| panic!("{editor} completion omitted fixture function add"));
    assert_eq!(add["detail"], "fn add(left: Int, right: Int): Int");
    assert_eq!(add["documentation"], "Add two integer values.");
    assert_eq!(add["insertText"], "add(${1:left}, ${2:right})");
    assert_eq!(add["insertTextFormat"], 2);
}

pub fn assert_vim_completion_projection(items: &Value) {
    let items = items
        .as_array()
        .expect("vim-lsp completion projection list");
    let actual_keywords = items
        .iter()
        .filter(|item| item["kind"] == "keyword")
        .map(|item| item["abbr"].as_str().unwrap().to_owned())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        actual_keywords,
        expected_keywords(),
        "Vim omni completion inventory"
    );
    assert!(
        items.iter().any(|item| item["abbr"] == "add~"),
        "Vim completion adapter omitted the add snippet candidate"
    );
}
