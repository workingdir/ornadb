//! Curated language reference data for hover documentation.
//!
//! The tables describe every keyword and standard scalar type in the Orna
//! language. Hovers render them with an example and a link to the grammar
//! specification, mirroring the rich reference docs of rust-analyzer.

/// One keyword reference entry.
#[derive(Debug, Clone, Copy)]
pub struct KeywordReference {
    /// The keyword in canonical upper case.
    pub keyword: &'static str,
    /// What the keyword does.
    pub summary: &'static str,
    /// Where the keyword appears in the grammar.
    pub context: &'static str,
    /// A one-line source example.
    pub example: &'static str,
}

/// One standard scalar type reference entry.
#[derive(Debug, Clone, Copy)]
pub struct ScalarReference {
    /// The canonical prelude spelling.
    pub name: &'static str,
    /// What the type stores.
    pub summary: &'static str,
    /// A one-line source example.
    pub example: &'static str,
}

/// Returns the reference entry for one keyword, case-insensitively.
pub fn keyword_reference(word: &str) -> Option<&'static KeywordReference> {
    let upper = word.to_ascii_uppercase();
    KEYWORD_REFERENCES
        .iter()
        .find(|entry| entry.keyword == upper)
}

/// Returns the reference entry for one scalar or standard type name.
pub fn scalar_reference(word: &str) -> Option<&'static ScalarReference> {
    let upper = word.to_ascii_uppercase();
    SCALAR_REFERENCES.iter().find(|entry| entry.name == upper)
}

/// The keyword reference table, ordered by category.
const KEYWORD_REFERENCES: &[KeywordReference] = &[
    // Declaration
    KeywordReference {
        keyword: "CREATE",
        summary: "Declares a schema object: a schema, type, or function.",
        context: "Starts every CREATE statement.",
        example: "CREATE SCHEMA tasks;",
    },
    KeywordReference {
        keyword: "SCHEMA",
        summary: "Names a namespace that groups types and functions.",
        context: "CREATE SCHEMA qualified_name; or the first part of any qualified name.",
        example: "CREATE SCHEMA tasks;",
    },
    KeywordReference {
        keyword: "TYPE",
        summary: "Declares an object, enum, or value type.",
        context: "CREATE TYPE qualified_name AS OBJECT | ENUM | VALUE ...;",
        example: "CREATE TYPE tasks.task AS OBJECT (title TEXT);",
    },
    KeywordReference {
        keyword: "AS",
        summary: "Introduces the kind of a type or the body of a function.",
        context: "AS OBJECT, AS ENUM, AS VALUE; function body after RETURNS.",
        example: "CREATE TYPE tasks.status AS ENUM ('open', 'done');",
    },
    KeywordReference {
        keyword: "OBJECT",
        summary: "Declares a durable object type stored in the catalog.",
        context: "CREATE TYPE ... AS OBJECT ( fields );",
        example: "CREATE TYPE tasks.task AS OBJECT (title TEXT NOT NULL);",
    },
    KeywordReference {
        keyword: "ENUM",
        summary: "Declares an ordered set of string labels.",
        context: "CREATE TYPE ... AS ENUM ( 'label', ... );",
        example: "CREATE TYPE tasks.status AS ENUM ('open', 'done');",
    },
    KeywordReference {
        keyword: "VALUE",
        summary: "Declares a non-object value type: record, primitive, or opaque.",
        context: "CREATE TYPE ... AS VALUE ( fields ) | OPAQUE | PRIMITIVE.",
        example: "CREATE TYPE tasks.money AS VALUE (amount DECIMAL);",
    },
    KeywordReference {
        keyword: "OPAQUE",
        summary: "Declares a value type with no visible fields, backed by a codec.",
        context: "CREATE TYPE ... AS VALUE OPAQUE KERNEL CONTRACT '...' IMMUTABLE TRANSIENT;",
        example: "CREATE TYPE std.io.ByteStream AS VALUE OPAQUE KERNEL CONTRACT 'stream@1' IMMUTABLE TRANSIENT;",
    },
    KeywordReference {
        keyword: "PRIMITIVE",
        summary: "Declares a kernel-backed scalar value type.",
        context: "CREATE TYPE ... AS VALUE PRIMITIVE KERNEL CONTRACT '...' IMMUTABLE PERSISTABLE;",
        example: "CREATE TYPE std.types.INTEGER AS VALUE PRIMITIVE KERNEL CONTRACT 'int@1' IMMUTABLE PERSISTABLE;",
    },
    KeywordReference {
        keyword: "FINAL",
        summary: "Marks an object type as closed to future fields.",
        context: "Trailing modifier of CREATE TYPE ... AS OBJECT.",
        example: "CREATE TYPE tasks.task AS OBJECT (title TEXT) FINAL;",
    },
    KeywordReference {
        keyword: "DOCUMENTATION",
        summary: "Attaches human-readable documentation to a declaration.",
        context: "Modifier on types, fields, and parameters; the string feeds LSP hovers.",
        example: "CREATE TYPE tasks.task AS OBJECT (title TEXT DOCUMENTATION 'the title');",
    },
    KeywordReference {
        keyword: "EXPORT",
        summary: "Publishes a type under a second name or into the prelude.",
        context: "EXPORT TYPE source AS target; EXPORT TYPE source TO PRELUDE AS name;",
        example: "EXPORT TYPE std.types.BOOLEAN AS std.BOOLEAN;",
    },
    KeywordReference {
        keyword: "PRELUDE",
        summary: "Targets the prelude namespace of unqualified scalar names.",
        context: "EXPORT TYPE ... TO PRELUDE AS name;",
        example: "EXPORT TYPE std.BOOLEAN TO PRELUDE AS BOOL;",
    },
    KeywordReference {
        keyword: "KERNEL",
        summary: "Marks a privileged value type as kernel-backed.",
        context: "KERNEL CONTRACT '...' inside primitive and opaque type declarations.",
        example: "CREATE TYPE std.types.UUID AS VALUE PRIMITIVE KERNEL CONTRACT 'uuid@1' IMMUTABLE PERSISTABLE;",
    },
    KeywordReference {
        keyword: "CONTRACT",
        summary: "Names the codec or representation contract of a value type.",
        context: "KERNEL CONTRACT '...' in privileged type declarations.",
        example: "KERNEL CONTRACT 'orna.kernel.value.boolean@1'",
    },
    KeywordReference {
        keyword: "SERVER",
        summary: "Declares a function that executes in the trusted database kernel.",
        context: "CREATE SERVER FUNCTION qualified_name ( params ) RETURNS type body;",
        example: "CREATE SERVER FUNCTION tasks.overdue (p_before TIMESTAMP) RETURNS ROWS (...) AS SELECT ...;",
    },
    KeywordReference {
        keyword: "CLIENT",
        summary: "Declares a function that executes in the local orna client.",
        context: "CREATE CLIENT FUNCTION qualified_name ( params ) RETURNS type body;",
        example: "CREATE CLIENT FUNCTION studio.main() RETURNS std.ui.UI AS std.ui.window(...);",
    },
    KeywordReference {
        keyword: "EXTERNAL",
        summary: "Declares a function backed by an external runtime contract.",
        context: "CREATE EXTERNAL CLIENT FUNCTION ... RUNTIME CONTRACT '...';",
        example: "CREATE EXTERNAL CLIENT FUNCTION runtime.tty () RETURNS std.terminal.Document RUNTIME CONTRACT 'tty@1';",
    },
    KeywordReference {
        keyword: "FUNCTION",
        summary: "Declares the executable unit of an Orna program.",
        context: "CREATE SERVER FUNCTION, CREATE CLIENT FUNCTION, CREATE EXTERNAL CLIENT FUNCTION.",
        example: "CREATE SERVER FUNCTION tasks.count() RETURNS BIGINT AS ...;",
    },
    KeywordReference {
        keyword: "RUNTIME",
        summary: "Names the runtime contract of an external client function.",
        context: "CREATE EXTERNAL CLIENT FUNCTION ... RUNTIME CONTRACT '...';",
        example: "RUNTIME CONTRACT 'orna.runtime.tty@1'",
    },
    KeywordReference {
        keyword: "RETURNS",
        summary: "Declares the result shape of a function.",
        context: "RETURNS type or RETURNS ROWS ( columns ) after the parameter list.",
        example: "RETURNS ROWS (task REF tasks.task, title TEXT)",
    },
    KeywordReference {
        keyword: "ROWS",
        summary: "Declares the named columns of a query-producing function.",
        context: "RETURNS ROWS ( column type, ... );",
        example: "RETURNS ROWS (title TEXT, due_at TIMESTAMP)",
    },
    KeywordReference {
        keyword: "TABLE",
        summary: "Table-valued return shape; the implementation uses ROWS.",
        context: "RETURNS TABLE ( columns ) is rejected; write RETURNS ROWS.",
        example: "RETURNS ROWS (created REF tasks.task)",
    },
    KeywordReference {
        keyword: "SECURITY",
        summary: "Selects the execution security context of a server function.",
        context: "SECURITY INVOKER or SECURITY DEFINER after RETURNS.",
        example: "SECURITY INVOKER",
    },
    KeywordReference {
        keyword: "INVOKER",
        summary: "Runs a function with the privileges of the calling principal.",
        context: "SECURITY INVOKER on server functions.",
        example: "CREATE SERVER FUNCTION tasks.view() RETURNS ROWS (...) SECURITY INVOKER AS ...;",
    },
    KeywordReference {
        keyword: "DEFINER",
        summary: "Runs a function with the privileges of its owner.",
        context: "SECURITY DEFINER on server functions.",
        example: "SECURITY DEFINER",
    },
    KeywordReference {
        keyword: "TRANSACTION",
        summary: "Declares the transaction behaviour of a server function.",
        context: "TRANSACTION ATOMIC | READ ONLY | MANUAL.",
        example: "TRANSACTION READ ONLY",
    },
    KeywordReference {
        keyword: "ATOMIC",
        summary: "Runs the function in one atomic transaction.",
        context: "TRANSACTION ATOMIC.",
        example: "TRANSACTION ATOMIC",
    },
    KeywordReference {
        keyword: "READ",
        summary: "Marks a transaction as read-only.",
        context: "TRANSACTION READ ONLY.",
        example: "TRANSACTION READ ONLY",
    },
    KeywordReference {
        keyword: "ONLY",
        summary: "Completes the read-only transaction clause.",
        context: "TRANSACTION READ ONLY.",
        example: "TRANSACTION READ ONLY",
    },
    KeywordReference {
        keyword: "MANUAL",
        summary: "Leaves transaction control to the function body.",
        context: "TRANSACTION MANUAL.",
        example: "TRANSACTION MANUAL",
    },
    KeywordReference {
        keyword: "VOLATILITY",
        summary: "Declares how aggressively a server function may be cached.",
        context: "VOLATILITY IMMUTABLE | STABLE | VOLATILE.",
        example: "VOLATILITY STABLE",
    },
    KeywordReference {
        keyword: "IMMUTABLE",
        summary: "Marks a function whose result never changes for the same inputs.",
        context: "VOLATILITY IMMUTABLE; also a value-type modifier.",
        example: "VOLATILITY IMMUTABLE",
    },
    KeywordReference {
        keyword: "STABLE",
        summary: "Marks a function that reads the database but writes nothing.",
        context: "VOLATILITY STABLE.",
        example: "VOLATILITY STABLE",
    },
    KeywordReference {
        keyword: "VOLATILE",
        summary: "Marks a function that can change the database.",
        context: "VOLATILITY VOLATILE.",
        example: "VOLATILITY VOLATILE",
    },
    KeywordReference {
        keyword: "REQUIRES",
        summary: "Declares the capabilities a function needs.",
        context: "REQUIRES CAPABILITY qualified_name ( args ).",
        example: "REQUIRES CAPABILITY std.terminal.write",
    },
    KeywordReference {
        keyword: "CAPABILITY",
        summary: "Names the capability set required by a function.",
        context: "REQUIRES CAPABILITY list.",
        example: "REQUIRES CAPABILITY sys.invoke",
    },
    KeywordReference {
        keyword: "ALTER",
        summary: "Changes a type or function in place.",
        context: "ALTER TYPE ... RENAME | ADD FIELD | DROP FIELD; ALTER FUNCTION ... RENAME TO.",
        example: "ALTER TYPE tasks.task RENAME FIELD title TO heading;",
    },
    KeywordReference {
        keyword: "RENAME",
        summary: "Renames a type, function, or field.",
        context: "ALTER TYPE ... RENAME TO; RENAME FIELD old TO new; ALTER FUNCTION ... RENAME TO.",
        example: "ALTER TYPE tasks.task RENAME TO tasks.item;",
    },
    KeywordReference {
        keyword: "FIELD",
        summary: "Names a field of an object type in ALTER statements.",
        context: "RENAME FIELD old TO new; ADD FIELD definition; DROP FIELD name.",
        example: "ALTER TYPE tasks.task ADD FIELD completed BOOLEAN;",
    },
    KeywordReference {
        keyword: "ADD",
        summary: "Adds a field to an object type.",
        context: "ALTER TYPE ... ADD FIELD definition.",
        example: "ALTER TYPE tasks.task ADD FIELD note TEXT;",
    },
    KeywordReference {
        keyword: "DROP",
        summary: "Removes a declaration or a field.",
        context: "DROP TYPE|FUNCTION|SCHEMA qualified_name [CASCADE|RESTRICT]; ALTER TYPE ... DROP FIELD.",
        example: "DROP TYPE tasks.task CASCADE;",
    },
    KeywordReference {
        keyword: "CASCADE",
        summary: "Propagates a drop to dependent objects.",
        context: "DROP ... CASCADE.",
        example: "DROP SCHEMA tasks CASCADE;",
    },
    KeywordReference {
        keyword: "RESTRICT",
        summary: "Rejects a drop while dependent objects exist.",
        context: "DROP ... RESTRICT.",
        example: "DROP TYPE tasks.task RESTRICT;",
    },
    KeywordReference {
        keyword: "USER",
        summary: "Declares a login principal.",
        context: "CREATE USER identifier [DISABLED]; also STATE ... SCOPE USER.",
        example: "CREATE USER bob;",
    },
    KeywordReference {
        keyword: "ROLE",
        summary: "Declares a group principal for grants.",
        context: "CREATE ROLE identifier; GRANT role TO principal.",
        example: "CREATE ROLE developer;",
    },
    KeywordReference {
        keyword: "DISABLED",
        summary: "Creates a principal that cannot sign in.",
        context: "CREATE USER ... DISABLED.",
        example: "CREATE USER pending DISABLED;",
    },
    KeywordReference {
        keyword: "GRANT",
        summary: "Grants a role or a privilege.",
        context: "GRANT role TO principal; GRANT privilege ON securable TO principal.",
        example: "GRANT EXECUTE ON FUNCTION studio.main TO developer;",
    },
    KeywordReference {
        keyword: "REVOKE",
        summary: "Removes a privilege.",
        context: "REVOKE privilege ON securable FROM principal.",
        example: "REVOKE EXECUTE ON FUNCTION studio.main FROM developer;",
    },
    KeywordReference {
        keyword: "EXECUTE",
        summary: "The privilege to invoke a function.",
        context: "GRANT EXECUTE ON FUNCTION ... TO ...;",
        example: "GRANT EXECUTE ON FUNCTION tasks.view TO developer;",
    },
    KeywordReference {
        keyword: "INSPECT",
        summary: "The privilege to inspect an object.",
        context: "GRANT INSPECT ON TYPE ... TO ...;",
        example: "GRANT INSPECT ON SCHEMA tasks TO auditor;",
    },
    KeywordReference {
        keyword: "TO",
        summary: "Names the grant target, rename target, or prelude target.",
        context: "GRANT ... TO principal; RENAME ... TO name; EXPORT ... TO PRELUDE.",
        example: "GRANT developer TO bob;",
    },
    KeywordReference {
        keyword: "FROM",
        summary: "Names the source of a revoke or a query source.",
        context: "REVOKE ... FROM principal; SELECT ... FROM object.",
        example: "SELECT t.title FROM tasks.task t;",
    },
    KeywordReference {
        keyword: "ON",
        summary: "Names the securable of a grant or a join condition.",
        context: "GRANT ... ON FUNCTION|TYPE|SCHEMA; JOIN ... ON condition; ON DELETE policy.",
        example: "GRANT SELECT ON TYPE tasks.task TO analyst;",
    },
    // Procedural
    KeywordReference {
        keyword: "IS",
        summary: "Introduces the declarative body of a function.",
        context: "IS declarations BEGIN statements END; after RETURNS.",
        example: "IS\n  LET v TIMESTAMP := sys.time.now();\nBEGIN\n  RETURN v;\nEND;",
    },
    KeywordReference {
        keyword: "BEGIN",
        summary: "Starts the statement section of a procedural body.",
        context: "IS declarations BEGIN statements END;",
        example: "BEGIN\n  RETURN 1;\nEND;",
    },
    KeywordReference {
        keyword: "END",
        summary: "Closes a procedural body, IF, or LOOP.",
        context: "END; END IF; END LOOP; END CASE.",
        example: "END IF;",
    },
    KeywordReference {
        keyword: "LET",
        summary: "Declares a local variable, optionally initialised.",
        context: "LET name type_spec [:= expression]; inside IS ... BEGIN.",
        example: "LET v TIMESTAMP := sys.time.now();",
    },
    KeywordReference {
        keyword: "CONST",
        summary: "Declares an immutable local constant.",
        context: "CONST name type_spec := expression;",
        example: "CONST c INT := 42;",
    },
    KeywordReference {
        keyword: "STATE",
        summary: "Declares durable state for a function instance.",
        context: "STATE name type_spec [SCOPE LOCAL|SESSION|USER] [DEFAULT expression];",
        example: "STATE filter TEXT SCOPE USER DEFAULT '';",
    },
    KeywordReference {
        keyword: "SCOPE",
        summary: "Selects the lifetime of declared state.",
        context: "STATE ... SCOPE LOCAL | SESSION | USER.",
        example: "STATE selection studio.catalog_node SCOPE SESSION;",
    },
    KeywordReference {
        keyword: "LOCAL",
        summary: "State that lives for one invocation.",
        context: "STATE ... SCOPE LOCAL.",
        example: "SCOPE LOCAL",
    },
    KeywordReference {
        keyword: "SESSION",
        summary: "State that lives for the client session.",
        context: "STATE ... SCOPE SESSION.",
        example: "SCOPE SESSION",
    },
    KeywordReference {
        keyword: "IF",
        summary: "Branches on a condition.",
        context: "IF expression THEN statements [ELSIF ...] [ELSE ...] END IF;",
        example: "IF v > 0 THEN RETURN TRUE; ELSE RETURN FALSE; END IF;",
    },
    KeywordReference {
        keyword: "THEN",
        summary: "Separates a condition from its branch body.",
        context: "IF ... THEN, CASE WHEN ... THEN, ELSIF ... THEN.",
        example: "IF done THEN RETURN 1; END IF;",
    },
    KeywordReference {
        keyword: "ELSIF",
        summary: "Adds another condition to an IF statement.",
        context: "IF ... ELSIF expression THEN ... END IF;",
        example: "ELSIF v = 0 THEN RETURN 0;",
    },
    KeywordReference {
        keyword: "ELSE",
        summary: "The fallback branch of IF or CASE.",
        context: "IF ... ELSE ... END IF; CASE ... ELSE ... END.",
        example: "ELSE RETURN NULL;",
    },
    KeywordReference {
        keyword: "WHILE",
        summary: "Repeats a body while a condition holds.",
        context: "WHILE expression LOOP statements END LOOP;",
        example: "WHILE n > 0 LOOP n := n - 1; END LOOP;",
    },
    KeywordReference {
        keyword: "LOOP",
        summary: "Repeats a body, completing the WHILE or FOR header.",
        context: "WHILE ... LOOP ... END LOOP; FOR ... LOOP ... END LOOP.",
        example: "END LOOP;",
    },
    KeywordReference {
        keyword: "FOR",
        summary: "Iterates over a value.",
        context: "FOR identifier IN expression LOOP statements END LOOP;",
        example: "FOR row IN rows LOOP CALL handle(row); END LOOP;",
    },
    KeywordReference {
        keyword: "IN",
        summary: "Introduces the iterated value, a membership test, or a query source.",
        context: "FOR x IN expr; x IN (list); ... IN ...",
        example: "FOR t IN tasks LOOP ... END LOOP;",
    },
    KeywordReference {
        keyword: "RETURN",
        summary: "Returns a value from a function.",
        context: "RETURN [expression]; inside a procedural body.",
        example: "RETURN p_task;",
    },
    KeywordReference {
        keyword: "RETURNING",
        summary: "Returns the affected rows of a mutation.",
        context: "INSERT ... RETURNING expression; UPDATE ... RETURNING.",
        example: "INSERT INTO tasks.task AS made (title) VALUES ('x') RETURNING REF(made);",
    },
    KeywordReference {
        keyword: "AWAIT",
        summary: "Waits for an asynchronous value.",
        context: "AWAIT expression; statement in a procedural body.",
        example: "AWAIT std.launch.run(entry);",
    },
    KeywordReference {
        keyword: "CALL",
        summary: "Invokes a function for its side effects.",
        context: "[CALL] invocation ; statement in a procedural body.",
        example: "CALL tasks.notify(p_task);",
    },
    KeywordReference {
        keyword: "CASE",
        summary: "Selects a value by matching conditions.",
        context: "CASE [expression] WHEN ... THEN ... [ELSE ...] END;",
        example: "CASE status WHEN 'open' THEN 1 ELSE 0 END",
    },
    KeywordReference {
        keyword: "WHEN",
        summary: "Gives a CASE branch condition.",
        context: "CASE ... WHEN expression THEN expression ...",
        example: "WHEN 'done' THEN 2",
    },
    // Expressions
    KeywordReference {
        keyword: "AND",
        summary: "Logical conjunction.",
        context: "expression AND expression.",
        example: "WHERE t.done = FALSE AND t.due_at < p_before",
    },
    KeywordReference {
        keyword: "OR",
        summary: "Logical disjunction.",
        context: "expression OR expression.",
        example: "WHERE a OR b",
    },
    KeywordReference {
        keyword: "NOT",
        summary: "Logical negation; also the nullable field modifier.",
        context: "NOT expression; field NOT NULL.",
        example: "title TEXT NOT NULL",
    },
    KeywordReference {
        keyword: "IS",
        summary: "Null and identity comparison.",
        context: "expression IS [NOT] NULL.",
        example: "WHERE p_task.assignee IS NULL",
    },
    KeywordReference {
        keyword: "NULL",
        summary: "The absent value.",
        context: "Literal; IS NULL comparison; field NULL modifier.",
        example: "DEFAULT NULL",
    },
    KeywordReference {
        keyword: "TRUE",
        summary: "The boolean true literal.",
        context: "Expression literal.",
        example: "VALUES (TRUE)",
    },
    KeywordReference {
        keyword: "FALSE",
        summary: "The boolean false literal.",
        context: "Expression literal.",
        example: "WHERE t.completed = FALSE",
    },
    KeywordReference {
        keyword: "LIKE",
        summary: "Pattern match on text.",
        context: "expression LIKE pattern.",
        example: "WHERE t.title LIKE 'fix%'",
    },
    KeywordReference {
        keyword: "ILIKE",
        summary: "Case-insensitive pattern match on text.",
        context: "expression ILIKE pattern.",
        example: "WHERE t.title ILIKE '%task%'",
    },
    KeywordReference {
        keyword: "DEFAULT",
        summary: "Gives a field or parameter its implicit value.",
        context: "Field modifier; parameter default; STATE DEFAULT.",
        example: "p_before TIMESTAMP DEFAULT sys.time.now()",
    },
    KeywordReference {
        keyword: "CHECK",
        summary: "Constrains a field value.",
        context: "CHECK ( expression ) field modifier.",
        example: "amount DECIMAL CHECK (amount >= 0)",
    },
    KeywordReference {
        keyword: "UNIQUE",
        summary: "Requires distinct field values.",
        context: "Field modifier.",
        example: "email TEXT UNIQUE",
    },
    KeywordReference {
        keyword: "NULLS",
        summary: "Controls null placement in ordering.",
        context: "ORDER BY ... NULLS FIRST | LAST.",
        example: "ORDER BY t.due_at NULLS LAST",
    },
    KeywordReference {
        keyword: "FIRST",
        summary: "Orders nulls first.",
        context: "NULLS FIRST in ORDER BY.",
        example: "NULLS FIRST",
    },
    KeywordReference {
        keyword: "LAST",
        summary: "Orders nulls last.",
        context: "NULLS LAST in ORDER BY.",
        example: "NULLS LAST",
    },
    KeywordReference {
        keyword: "BETWEEN",
        summary: "Range membership test.",
        context: "expression BETWEEN expression AND expression.",
        example: "WHERE t.due_at BETWEEN p_start AND p_end",
    },
    KeywordReference {
        keyword: "EXISTS",
        summary: "Tests whether a query produces rows.",
        context: "EXISTS ( query ).",
        example: "WHERE EXISTS (SELECT ... FROM ...)",
    },
    KeywordReference {
        keyword: "DISTINCT",
        summary: "Removes duplicate rows.",
        context: "SELECT DISTINCT ...",
        example: "SELECT DISTINCT t.status FROM tasks.task t;",
    },
    KeywordReference {
        keyword: "ALL",
        summary: "Compares against every row of a subquery.",
        context: "expression operator ALL ( query ).",
        example: "WHERE x > ALL (SELECT ...)",
    },
    KeywordReference {
        keyword: "UNION",
        summary: "Combines the rows of two queries.",
        context: "query UNION [ALL] query.",
        example: "SELECT a FROM t UNION SELECT b FROM u;",
    },
    // Query
    KeywordReference {
        keyword: "SELECT",
        summary: "Projects columns from an object source.",
        context: "SELECT columns FROM object [WHERE ...] [ORDER BY ...];",
        example: "SELECT t.title FROM tasks.task t WHERE t.done = FALSE;",
    },
    KeywordReference {
        keyword: "INSERT",
        summary: "Adds a row to an object type.",
        context: "INSERT INTO object AS alias ( columns ) VALUES ( ... ) RETURNING ...;",
        example: "INSERT INTO tasks.task AS made (title) VALUES ('x') RETURNING REF(made);",
    },
    KeywordReference {
        keyword: "UPDATE",
        summary: "Changes the fields of a selected object.",
        context: "UPDATE object alias SET field = expression WHERE ...;",
        example: "UPDATE tasks.task t SET completed = TRUE WHERE REF(t) = p_task;",
    },
    KeywordReference {
        keyword: "DELETE",
        summary: "Removes a selected object.",
        context: "DELETE FROM object alias WHERE ...;",
        example: "DELETE FROM tasks.task t WHERE REF(t) = p_task;",
    },
    KeywordReference {
        keyword: "VALUES",
        summary: "Supplies the rows of an INSERT.",
        context: "INSERT ... VALUES ( expression, ... ).",
        example: "VALUES (TRUE)",
    },
    KeywordReference {
        keyword: "INTO",
        summary: "Names the target object of an INSERT.",
        context: "INSERT INTO qualified_name ...",
        example: "INSERT INTO tasks.task AS made (title) VALUES ('x');",
    },
    KeywordReference {
        keyword: "SET",
        summary: "Assigns fields in an UPDATE; also the collection type.",
        context: "UPDATE ... SET field = expression; SET<type> type constructor.",
        example: "SET completed = TRUE",
    },
    KeywordReference {
        keyword: "WHERE",
        summary: "Filters rows by a predicate.",
        context: "SELECT/UPDATE/DELETE ... WHERE expression.",
        example: "WHERE t.due_at < p_before",
    },
    KeywordReference {
        keyword: "ORDER",
        summary: "Orders the result rows.",
        context: "ORDER BY expressions.",
        example: "ORDER BY t.due_at",
    },
    KeywordReference {
        keyword: "BY",
        summary: "Completes ORDER BY and GROUP BY.",
        context: "ORDER BY column [ASC|DESC]; GROUP BY columns.",
        example: "ORDER BY t.due_at DESC",
    },
    KeywordReference {
        keyword: "GROUP",
        summary: "Groups rows for aggregation.",
        context: "GROUP BY columns.",
        example: "GROUP BY t.status",
    },
    KeywordReference {
        keyword: "HAVING",
        summary: "Filters grouped rows.",
        context: "HAVING aggregate condition.",
        example: "HAVING COUNT(*) > 0",
    },
    KeywordReference {
        keyword: "LIMIT",
        summary: "Caps the number of returned rows.",
        context: "LIMIT count.",
        example: "LIMIT 10",
    },
    KeywordReference {
        keyword: "OFFSET",
        summary: "Skips rows before returning results.",
        context: "OFFSET count.",
        example: "OFFSET 20",
    },
    KeywordReference {
        keyword: "ASC",
        summary: "Ascending order.",
        context: "ORDER BY ... ASC.",
        example: "ORDER BY t.title ASC",
    },
    KeywordReference {
        keyword: "DESC",
        summary: "Descending order.",
        context: "ORDER BY ... DESC.",
        example: "ORDER BY t.due_at DESC",
    },
    KeywordReference {
        keyword: "JOIN",
        summary: "Combines rows from two sources.",
        context: "source JOIN source ON condition.",
        example: "FROM tasks.task t JOIN tasks.project p ON t.project = p.id",
    },
    KeywordReference {
        keyword: "INNER",
        summary: "Inner join.",
        context: "INNER JOIN.",
        example: "INNER JOIN tasks.project p ON ...",
    },
    KeywordReference {
        keyword: "LEFT",
        summary: "Left outer join.",
        context: "LEFT JOIN.",
        example: "LEFT JOIN tasks.project p ON ...",
    },
    KeywordReference {
        keyword: "RIGHT",
        summary: "Right outer join.",
        context: "RIGHT JOIN.",
        example: "RIGHT JOIN ...",
    },
    KeywordReference {
        keyword: "OUTER",
        summary: "Outer join.",
        context: "LEFT/RIGHT/FULL OUTER JOIN.",
        example: "FULL OUTER JOIN ...",
    },
    KeywordReference {
        keyword: "FULL",
        summary: "Full outer join.",
        context: "FULL JOIN.",
        example: "FULL JOIN ...",
    },
    KeywordReference {
        keyword: "CROSS",
        summary: "Cross join.",
        context: "CROSS JOIN.",
        example: "CROSS JOIN tasks.task",
    },
    // Types
    KeywordReference {
        keyword: "REF",
        summary: "A typed reference to a durable object.",
        context: "REF type; also REF(alias) to take an object reference.",
        example: "assignee REF tasks.task",
    },
    KeywordReference {
        keyword: "LIST",
        summary: "An ordered list value.",
        context: "LIST<type> type constructor.",
        example: "tags LIST<TEXT>",
    },
    KeywordReference {
        keyword: "MAP",
        summary: "A key-to-value map value.",
        context: "MAP<key_type, value_type> type constructor.",
        example: "counts MAP<TEXT, INT>",
    },
    KeywordReference {
        keyword: "STREAM",
        summary: "A lazy sequence of values.",
        context: "STREAM<type> type constructor.",
        example: "events STREAM<std.event>",
    },
    KeywordReference {
        keyword: "OPTION",
        summary: "An optional value; either present or NULL.",
        context: "OPTION<type> type constructor, equivalent to type?.",
        example: "nickname OPTION<TEXT>",
    },
];

/// The scalar type reference table, covering every prelude spelling.
const SCALAR_REFERENCES: &[ScalarReference] = &[
    ScalarReference {
        name: "BOOL",
        summary: "The boolean type: TRUE or FALSE.",
        example: "done BOOL NOT NULL",
    },
    ScalarReference {
        name: "BOOLEAN",
        summary: "The boolean type: TRUE or FALSE.",
        example: "done BOOLEAN NOT NULL",
    },
    ScalarReference {
        name: "INT",
        summary: "A 32-bit signed integer.",
        example: "count INT DEFAULT 0",
    },
    ScalarReference {
        name: "INTEGER",
        summary: "A 32-bit signed integer.",
        example: "count INTEGER DEFAULT 0",
    },
    ScalarReference {
        name: "BIGINT",
        summary: "A 64-bit signed integer.",
        example: "total BIGINT",
    },
    ScalarReference {
        name: "FLOAT",
        summary: "A binary floating-point number.",
        example: "score FLOAT",
    },
    ScalarReference {
        name: "DECIMAL",
        summary: "An exact decimal number for money and measurements.",
        example: "amount DECIMAL",
    },
    ScalarReference {
        name: "TEXT",
        summary: "A variable-length character string.",
        example: "title TEXT NOT NULL",
    },
    ScalarReference {
        name: "CHARACTER_LARGE_OBJECT",
        summary: "The canonical character large object; prelude spelling TEXT.",
        example: "body CHARACTER LARGE OBJECT",
    },
    ScalarReference {
        name: "BYTES",
        summary: "A binary byte string.",
        example: "payload BYTES",
    },
    ScalarReference {
        name: "BINARY_LARGE_OBJECT",
        summary: "The canonical binary large object; prelude spelling BYTES.",
        example: "data BINARY LARGE OBJECT",
    },
    ScalarReference {
        name: "UUID",
        summary: "A universally unique identifier.",
        example: "id UUID NOT NULL",
    },
    ScalarReference {
        name: "DATE",
        summary: "A calendar date.",
        example: "due_on DATE",
    },
    ScalarReference {
        name: "TIME",
        summary: "A time of day.",
        example: "starts_at TIME",
    },
    ScalarReference {
        name: "TIMESTAMP",
        summary: "A date and time, optionally with time zone.",
        example: "due_at TIMESTAMP",
    },
    ScalarReference {
        name: "DURATION",
        summary: "A length of time between instants.",
        example: "timeout DURATION",
    },
    ScalarReference {
        name: "VOID",
        summary: "The type of a function that returns nothing.",
        example: "RETURNS VOID",
    },
];
