//! A strict reader for the subset of TOML the two ratchet files use.
//!
//! The repository has no TOML dependency (`docs/design/hir.md` section 7.4
//! records the absence as a constraint on new code), and the two ratchet files
//! need only arrays of tables whose values are strings, integers or booleans.
//! Rather than depend on a parser, this module reads exactly that subset and
//! rejects everything else, so a typo in a ratchet entry is a load error
//! instead of a silently ignored line.

/// One parsed file: its tables in file order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Document {
    /// Tables in the order they appear.
    pub tables: Vec<Table>,
}

/// One `[[name]]` table and its entries.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Table {
    /// The table's name.
    pub name: String,
    /// Its `key = value` entries in file order.
    pub entries: Vec<(String, Value)>,
}

/// A scalar value in the supported subset.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Value {
    /// A basic string.
    Str(String),
    /// An integer.
    Int(i64),
    /// A boolean.
    Bool(bool),
}

impl Document {
    /// Returns every table with `name`.
    pub fn tables_named<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Table> {
        self.tables.iter().filter(move |table| table.name == name)
    }
}

impl Table {
    /// Returns a required string entry.
    pub fn string(&self, key: &str) -> Result<&str, String> {
        match self.value(key)? {
            Value::Str(value) => Ok(value),
            other => Err(format!(
                "{}: `{key}` must be a string, found {other:?}",
                self.name
            )),
        }
    }

    /// Returns a required integer entry.
    pub fn integer(&self, key: &str) -> Result<i64, String> {
        match self.value(key)? {
            Value::Int(value) => Ok(*value),
            other => Err(format!(
                "{}: `{key}` must be an integer, found {other:?}",
                self.name
            )),
        }
    }

    fn value(&self, key: &str) -> Result<&Value, String> {
        self.entries
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value)
            .ok_or_else(|| format!("{}: missing `{key}`", self.name))
    }
}

/// Parses one ratchet file.
pub fn parse(source: &str) -> Result<Document, String> {
    let mut tables = Vec::new();
    let mut current: Option<Table> = None;
    for (number, line) in source.lines().enumerate() {
        let line = strip_comment(line).trim();
        if line.is_empty() {
            continue;
        }
        let line_number = number + 1;
        if let Some(name) = line
            .strip_prefix("[[")
            .and_then(|rest| rest.strip_suffix("]]"))
        {
            let name = name.trim();
            if name.is_empty() {
                return Err(format!("line {line_number}: empty table name"));
            }
            if let Some(table) = current.take() {
                tables.push(table);
            }
            current = Some(Table {
                name: name.to_owned(),
                entries: Vec::new(),
            });
            continue;
        }
        if line.starts_with('[') {
            return Err(format!(
                "line {line_number}: only `[[name]]` tables are supported"
            ));
        }
        let (key, value) = line
            .split_once('=')
            .ok_or_else(|| format!("line {line_number}: expected `key = value`"))?;
        let key = key.trim();
        if key.is_empty()
            || !key.chars().all(|character| {
                character.is_ascii_alphanumeric() || character == '_' || character == '-'
            })
        {
            return Err(format!("line {line_number}: unsupported key `{key}`"));
        }
        let value = parse_value(value.trim(), line_number)?;
        let table = current
            .as_mut()
            .ok_or_else(|| format!("line {line_number}: entry outside a table"))?;
        if table.entries.iter().any(|(name, _)| name == key) {
            return Err(format!("line {line_number}: duplicate key `{key}`"));
        }
        table.entries.push((key.to_owned(), value));
    }
    if let Some(table) = current {
        tables.push(table);
    }
    Ok(Document { tables })
}

/// Removes a trailing `#` comment, respecting comments inside basic strings.
fn strip_comment(line: &str) -> &str {
    let mut in_string = false;
    let mut escaped = false;
    for (index, character) in line.char_indices() {
        if in_string {
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == '"' {
                in_string = false;
            }
            continue;
        }
        match character {
            '"' => in_string = true,
            '#' => return &line[..index],
            _ => {}
        }
    }
    line
}

fn parse_value(text: &str, line_number: usize) -> Result<Value, String> {
    if let Some(body) = text
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
    {
        return unescape(body)
            .map(Value::Str)
            .map_err(|error| format!("line {line_number}: {error}"));
    }
    match text {
        "true" => return Ok(Value::Bool(true)),
        "false" => return Ok(Value::Bool(false)),
        _ => {}
    }
    text.parse::<i64>()
        .map(Value::Int)
        .map_err(|_| format!("line {line_number}: unsupported value `{text}`"))
}

fn unescape(body: &str) -> Result<String, String> {
    let mut output = String::with_capacity(body.len());
    let mut characters = body.chars();
    while let Some(character) = characters.next() {
        if character != '\\' {
            if character == '"' {
                return Err("unescaped `\"` in a basic string".to_owned());
            }
            output.push(character);
            continue;
        }
        match characters.next() {
            Some('"') => output.push('"'),
            Some('\\') => output.push('\\'),
            Some('n') => output.push('\n'),
            Some('t') => output.push('\t'),
            Some('r') => output.push('\r'),
            Some(other) => return Err(format!("unsupported escape `\\{other}`")),
            None => return Err("trailing backslash in a basic string".to_owned()),
        }
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::{Value, parse};

    #[test]
    fn reads_arrays_of_tables_with_scalars() {
        let document = parse(
            "# a comment\n[[entry]]\ncase = \"a \\\"quoted\\\" id\"\ncount = 42\nstrict = true\n\n[[entry]]\nsame = \"b # not a comment\"\n",
        )
        .expect("the subset parses");
        assert_eq!(document.tables.len(), 2);
        let first = &document.tables[0];
        assert_eq!(first.name, "entry");
        assert_eq!(first.string("case").expect("string"), "a \"quoted\" id");
        assert_eq!(first.integer("count").expect("integer"), 42);
        assert_eq!(first.entries[2].1, Value::Bool(true));
        assert_eq!(
            document.tables[1].string("same").expect("string"),
            "b # not a comment"
        );
    }

    #[test]
    fn rejects_what_it_does_not_read() {
        for source in [
            "[single]\nkey = \"value\"\n",
            "key = \"value\"\n",
            "[[entry]]\nkey = [1, 2]\n",
            "[[entry]]\nkey = \"unterminated\n",
            "[[entry]]\nkey = \"value\"\nkey = \"again\"\n",
            "[[entry]]\nbad key = 1\n",
        ] {
            assert!(parse(source).is_err(), "must reject {source:?}");
        }
    }

    #[test]
    fn requires_every_named_key() {
        let document = parse("[[entry]]\ncase = \"x\"\n").expect("parses");
        let table = &document.tables[0];
        assert!(table.string("reason").is_err());
        assert!(table.string("case").is_ok());
    }
}
