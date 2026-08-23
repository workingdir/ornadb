/*
  * tree-sitter grammar for the Orna language.
  *
  * Based on spec/spec/orna.ebnf (design v0.2) plus the real-world sources in
  * spec/examples, crates/orna-system-tests/fixtures and stdlib/std.
  *
  * Design notes:
  * - Keywords are matched case-insensitively (regex /i flag) and are declared
  *   before identifiers so that equal-length lexer ties resolve to keywords.
  * - Unquoted identifiers are case-sensitive; keywords are additionally usable
  *   as name components (qualified names such as std.types.DATE and prelude
  *   names such as "CHARACTER LARGE OBJECT" contain keyword-shaped words).
  * - SQL bodies (SELECT/INSERT/UPDATE/DELETE) are first-class statements.
  * - No word-boundary assertions are used: tree-sitter's regex engine does not
  *   support \b; the lexer's longest-match rule keeps `created` an identifier
  *   while `CREATE` is a keyword.
  */

const KEYWORDS = [
    'create', 'schema', 'type', 'as', 'enum', 'object', 'value', 'opaque',
    'primitive', 'final', 'documentation', 'immutable', 'transient',
    'persistable', 'sealed', 'kernel', 'contract', 'export', 'to', 'prelude',
    'server', 'function', 'client', 'external', 'returns', 'table', 'rows',
    'runtime',
    'security', 'invoker', 'definer', 'transaction', 'atomic', 'read', 'only',
    'manual', 'volatility', 'stable', 'volatile', 'requires', 'capability',
    'alter', 'rename', 'field', 'add', 'drop', 'cascade', 'restrict', 'user',
    'disabled', 'role', 'grant', 'revoke', 'on', 'from', 'execute', 'select',
    'insert', 'update', 'delete', 'inspect', 'is', 'begin', 'end', 'if',
    'then', 'elsif', 'else', 'while', 'loop', 'for', 'in', 'let', 'const',
    'state', 'scope', 'local', 'session', 'default', 'not', 'null', 'unique',
    'check', 'call', 'await', 'return', 'and', 'or', 'like', 'ilike', 'true',
    'false', 'case', 'when', 'ref', 'list', 'set', 'map', 'stream', 'option',
    'boolean', 'bool', 'integer', 'int', 'bigint', 'float', 'decimal',
    'character', 'large', 'text', 'binary', 'bytes', 'uuid',
    'date', 'time', 'timestamp', 'duration', 'void', 'distinct', 'order',
    'by', 'values', 'returning', 'into', 'where',
];

// Build one named, case-insensitive token rule per keyword. `token` is the
// tree-sitter helper; the regex /i flag makes keywords case-insensitive and
// the explicit precedence makes keywords win lexer ties against identifiers
// regardless of rule definition order.
const keywordRules = {};
for (const word of KEYWORDS) {
    keywordRules['kw_' + word] = ($) => token(prec(100, new RegExp(word, 'i')));
}

function sep1(rule, separator) {
    return seq(rule, repeat(seq(separator, rule)));
}

function sep(rule, separator) {
    return optional(sep1(rule, separator));
}

function sep_trailing(rule, separator) {
    return optional(seq(sep1(rule, separator), optional(separator)));
}

module.exports = grammar({
    name: 'orna',

    extras: ($) => [$.comment, /[\s\uFEFF\u2060]/],

    word: ($) => $._unquoted_identifier,

    conflicts: ($) => [
        // A single identifier inside parentheses is both a parenthesized
        // expression (qualified name) and a lambda parameter pattern.
        [$._name_first, $._name],
        // `REF` starts both a reference type (REF t) and a callable name (REF(x)).
        [$._type_base, $._name_first],
        // ELSIF clauses vs statement repetition inside IF bodies.
        [$.if_statement],
        // A dotted CLIENT call callee and a parameter field path share the
        // same prefix until the call opening parenthesis.
        [$.client_call_callee, $.client_field_path],
    ],

    rules: Object.assign(
        {
            // ------------------------------------------------------------------
            // Entry point
            // ------------------------------------------------------------------
            source_file: ($) => repeat($._top_level_statement),

            _top_level_statement: ($) =>
                choice(
                    $.create_schema_statement,
                    $.create_type_statement,
                    $.export_type_statement,
                    $.create_server_function_statement,
                    $.create_client_function_statement,
                    $.create_external_client_function_statement,
                    $.alter_statement,
                    $.drop_statement,
                    $.create_user_statement,
                    $.create_role_statement,
                    $.grant_statement,
                    $.revoke_statement,
                    seq($.sql_body, $.semicolon),
                ),

            // ------------------------------------------------------------------
            // Schema and type DDL
            // ------------------------------------------------------------------
            create_schema_statement: ($) =>
                seq($.kw_create, $.kw_schema, field('name', $.qualified_name), $.semicolon),

            create_type_statement: ($) =>
                choice(
                    $.create_enum_type_statement,
                    $.create_object_type_statement,
                    $.create_value_type_statement,
                ),

            create_enum_type_statement: ($) =>
                seq(
                    $.kw_create,
                    $.kw_type,
                    field('name', $.qualified_name),
                    $.kw_as,
                    $.kw_enum,
                    $.lparen,
                    sep_trailing($.string_literal, $.comma),
                    $.rparen,
                    $.semicolon,
                ),

            create_object_type_statement: ($) =>
                seq(
                    $.kw_create,
                    $.kw_type,
                    field('name', $.qualified_name),
                    $.kw_as,
                    $.kw_object,
                    $.lparen,
                    sep_trailing($.field_definition, $.comma),
                    $.rparen,
                    repeat($._type_modifier),
                    $.semicolon,
                ),

            create_value_type_statement: ($) =>
                seq(
                    $.kw_create,
                    $.kw_type,
                    field('name', $.qualified_name),
                    $.kw_as,
                    $.kw_value,
                    choice(
                        seq($.lparen, sep_trailing($.field_definition, $.comma), $.rparen),
                        $.kw_opaque,
                        $.kw_primitive,
                    ),
                    repeat($._value_type_modifier),
                    $.semicolon,
                ),

            export_type_statement: ($) =>
                seq(
                    $.kw_export,
                    $.kw_type,
                    field('name', $.qualified_name),
                    choice(
                        seq($.kw_as, field('alias', $.qualified_name)),
                        seq(
                            $.kw_to,
                            $.kw_prelude,
                            $.kw_as,
                            field('prelude_name', repeat1($._prelude_name)),
                        ),
                    ),
                    $.semicolon,
                ),

            _type_modifier: ($) =>
                choice($.kw_final, seq($.kw_documentation, $.string_literal)),

            _value_type_modifier: ($) =>
                choice(
                    $.kw_immutable,
                    $.kw_transient,
                    $.kw_persistable,
                    $.kw_sealed,
                    seq($.kw_documentation, $.string_literal),
                    seq($.kw_kernel, $.kw_contract, $.string_literal),
                ),

            field_definition: ($) =>
                seq(
                    field('name', $._name),
                    field('type', $.type_spec),
                    repeat($._field_modifier),
                ),

            _field_modifier: ($) =>
                choice(
                    seq($.kw_not, $.kw_null),
                    $.kw_null,
                    seq($.kw_default, $.expression),
                    $.kw_unique,
                    seq($.kw_on, $.kw_delete, $._on_delete_action),
                    seq($.kw_check, $.lparen, $.expression, $.rparen),
                    seq($.kw_documentation, $.string_literal),
                ),

            _on_delete_action: ($) =>
                choice($.kw_restrict, seq($.kw_set, $.kw_null), $.kw_cascade),

            // ------------------------------------------------------------------
            // Function DDL
            // ------------------------------------------------------------------
            create_server_function_statement: ($) =>
                seq(
                    $.kw_create,
                    $.kw_server,
                    $.kw_function,
                    field('name', $.qualified_name),
                    field('parameters', $.parameter_list),
                    $.kw_returns,
                    field('returns', $.return_type_spec),
                    repeat(
                        choice(
                            $.security_clause,
                            $.transaction_clause,
                            $.volatility_clause,
                            $.capability_clause,
                        ),
                    ),
                    field('body', $.function_body),
                    $.semicolon,
                ),

            create_client_function_statement: ($) =>
                seq(
                    $.kw_create,
                    $.kw_client,
                    $.kw_function,
                    field('name', $.qualified_name),
                    field('parameters', $.parameter_list),
                    $.kw_returns,
                    field('returns', $.return_type_spec),
                    optional($.capability_clause),
                    field('body', $._client_function_body),
                    $.semicolon,
                ),

            create_external_client_function_statement: ($) =>
                seq(
                    $.kw_create,
                    $.kw_external,
                    $.kw_client,
                    $.kw_function,
                    field('name', $.qualified_name),
                    field('parameters', $.parameter_list),
                    $.kw_returns,
                    field('returns', $.return_type_spec),
                    $.kw_runtime,
                    $.kw_contract,
                    $.string_literal,
                    optional($.capability_clause),
                    $.semicolon,
                ),

            parameter_list: ($) => seq($.lparen, sep($.parameter_definition, $.comma), $.rparen),

            parameter_definition: ($) =>
                seq(
                    field('name', $._name),
                    field('type', $.type_spec),
                    optional(seq($.kw_default, field('default', $.expression))),
                    optional(seq($.kw_documentation, $.string_literal)),
                ),

            return_type_spec: ($) =>
                choice(
                    $.type_spec,
                    seq($.kw_rows, $.lparen, sep($.table_column_definition, $.comma), $.rparen),
                ),

            table_column_definition: ($) =>
                seq(field('name', $._name), field('type', $.type_spec)),

            security_clause: ($) => seq($.kw_security, choice($.kw_invoker, $.kw_definer)),

            transaction_clause: ($) =>
                seq($.kw_transaction, choice($.kw_atomic, seq($.kw_read, $.kw_only), $.kw_manual)),

            volatility_clause: ($) =>
                seq($.kw_volatility, choice($.kw_immutable, $.kw_stable, $.kw_volatile)),

            capability_clause: ($) =>
                seq($.kw_requires, $.kw_capability, sep1($.capability_spec, $.comma)),

            capability_spec: ($) =>
                seq($.qualified_name, optional(seq($.lparen, optional($.argument_list), $.rparen))),

            function_body: ($) =>
                choice(
                    seq($.kw_as, choice($.expression, $.sql_body)),
                    $.procedural_body,
                ),

            _client_function_body: ($) =>
                choice(
                    $.client_expression_body,
                    $.client_procedural_body,
                    $.client_return_body,
                ),

            client_expression_body: ($) =>
                seq($.kw_as, field('expression', $.client_expression)),

            client_return_body: ($) =>
                seq($.kw_return, field('expression', $.client_expression)),

            // CLIENT procedural blocks deliberately use a closed statement
            // surface instead of SERVER's generic procedural_body. A state
            // block has one or more STATE declarations, then BEGIN and exactly
            // one RETURN. A no-STATE block may start with typed LET locals and
            // has only LET or assignment statements before exactly one RETURN.
            client_procedural_body: ($) =>
                choice($.client_state_body, $.client_no_state_body),

            client_state_body: ($) =>
                seq(
                    $.kw_is,
                    repeat1($.client_state_declaration),
                    $.kw_begin,
                    $.client_return_statement,
                    $.kw_end,
                ),

            client_no_state_body: ($) =>
                seq(
                    $.kw_is,
                    repeat($.client_local_declaration),
                    $.kw_begin,
                    repeat($.client_procedural_statement),
                    $.client_return_statement,
                    $.kw_end,
                ),

            client_state_declaration: ($) =>
                seq(
                    $.kw_state,
                    field('name', $._name),
                    field('type', $.type_spec),
                    optional(seq($.kw_scope, choice($.kw_local, $.kw_session, $.kw_user))),
                    optional(seq($.kw_default, field('default', choice($.kw_null, $.client_expression)))),
                    $.semicolon,
                ),

            // Pre-BEGIN CLIENT locals require an explicit type and := value.
            client_local_declaration: ($) =>
                seq(
                    $.kw_let,
                    field('name', $._name),
                    field('type', $.type_spec),
                    $.assignment_operator,
                    field('value', choice($.client_await_expression, $.client_expression)),
                    $.semicolon,
                ),

            client_procedural_statement: ($) =>
                choice($.client_let_statement, $.client_assignment_statement),

            // Post-BEGIN LET may omit its type, unlike a pre-BEGIN local.
            client_let_statement: ($) =>
                seq(
                    $.kw_let,
                    field('name', $._name),
                    optional(field('type', $.type_spec)),
                    $.assignment_operator,
                    field('value', choice($.client_await_expression, $.client_expression)),
                    $.semicolon,
                ),

            client_assignment_statement: ($) =>
                seq(
                    field('target', $._name),
                    $.assignment_operator,
                    field('value', choice($.client_await_expression, $.client_expression)),
                    $.semicolon,
                ),

            client_return_statement: ($) =>
                seq(
                    $.kw_return,
                    optional(field('expression', choice($.client_await_expression, $.client_expression))),
                    $.semicolon,
                ),

            client_expression: ($) =>
                prec.left(
                    1,
                    choice(
                        seq($.client_expression, $.client_concat_operator, $.client_primary_expression),
                        $.client_primary_expression,
                    ),
                ),

            client_primary_expression: ($) =>
                choice(
                    $.client_call_expression,
                    $.string_literal,
                    $.client_integer_literal,
                    $.boolean_literal,
                    $.client_field_path,
                    $.client_parameter_read,
                ),

            // AWAIT is only valid in procedural LET, assignment, and RETURN
            // positions. Its operand remains a closed non-suspending CLIENT
            // expression; semantic checks restrict it to a resource value.
            client_await_expression: ($) =>
                seq($.kw_await, field('expression', $.client_expression)),

            client_call_expression: ($) =>
                seq(
                    field('callee', $.client_call_callee),
                    $.lparen,
                    optional($.client_argument_list),
                    $.rparen,
                ),

            client_call_callee: ($) =>
                prec(4, seq($._name_first, repeat(seq($.dot, $._name)))),

            client_argument_list: ($) => sep1($.client_argument, $.comma),

            client_argument: ($) =>
                choice(
                    prec(
                        2,
                        seq(
                            field('name', $._name),
                            $.arrow_operator,
                            field('value', $.client_expression),
                        ),
                    ),
                    prec(1, field('value', $.client_expression)),
                ),

            client_field_path: ($) =>
                prec(
                    3,
                    seq(
                        field('parameter', $._name_first),
                        repeat1(seq($.dot, field('field', $._name))),
                    ),
                ),

            client_parameter_read: ($) => $._name_first,

            client_integer_literal: ($) => token(/\d+/),

            client_concat_operator: ($) => token('||'),

            procedural_body: ($) =>
                seq(
                    $.kw_is,
                    repeat($._local_declaration),
                    $.kw_begin,
                    repeat($.procedural_statement),
                    $.kw_end,
                ),

            // ------------------------------------------------------------------
            // ALTER / DROP / users / roles / grants
            // ------------------------------------------------------------------
            alter_statement: ($) => choice($.alter_type_statement, $.alter_function_statement),

            alter_type_statement: ($) =>
                seq(
                    $.kw_alter,
                    $.kw_type,
                    field('name', $.qualified_name),
                    choice(
                        seq($.kw_rename, $.kw_to, field('new_name', $._name)),
                        seq(
                            $.kw_rename,
                            $.kw_field,
                            field('old_name', $._name),
                            $.kw_to,
                            field('new_name', $._name),
                        ),
                        seq($.kw_add, $.kw_field, field('field', $.field_definition)),
                        seq($.kw_drop, $.kw_field, field('name', $._name)),
                    ),
                    $.semicolon,
                ),

            alter_function_statement: ($) =>
                seq(
                    $.kw_alter,
                    $.kw_function,
                    field('name', $.qualified_name),
                    $.kw_rename,
                    $.kw_to,
                    field('new_name', $._name),
                    $.semicolon,
                ),

            drop_statement: ($) =>
                seq(
                    $.kw_drop,
                    choice($.kw_type, $.kw_function, $.kw_schema),
                    field('name', $.qualified_name),
                    optional(choice($.kw_cascade, $.kw_restrict)),
                    $.semicolon,
                ),

            create_user_statement: ($) =>
                seq($.kw_create, $.kw_user, field('name', $._name), optional($.kw_disabled), $.semicolon),

            create_role_statement: ($) =>
                seq($.kw_create, $.kw_role, field('name', $._name), $.semicolon),

            grant_statement: ($) => choice($.grant_role_statement, $.grant_privilege_statement),

            grant_role_statement: ($) =>
                seq(
                    $.kw_grant,
                    field('role', $.qualified_name),
                    $.kw_to,
                    field('grantee', $.qualified_name),
                    $.semicolon,
                ),

            grant_privilege_statement: ($) =>
                seq(
                    $.kw_grant,
                    field('privilege', $.privilege_spec),
                    $.kw_on,
                    field('securable', $.securable_spec),
                    $.kw_to,
                    field('grantee', $.qualified_name),
                    $.semicolon,
                ),

            revoke_statement: ($) =>
                seq(
                    $.kw_revoke,
                    field('privilege', $.privilege_spec),
                    $.kw_on,
                    field('securable', $.securable_spec),
                    $.kw_from,
                    field('grantee', $.qualified_name),
                    $.semicolon,
                ),

            // Higher precedence than _keyword so GRANT SELECT ON ... parses as a
            // privilege rather than a role named SELECT.
            privilege_spec: ($) =>
                prec(
                    2,
                    choice(
                        $.kw_execute,
                        $.kw_select,
                        $.kw_insert,
                        $.kw_update,
                        $.kw_delete,
                        $.kw_inspect,
                        $.qualified_name,
                    ),
                ),

            securable_spec: ($) =>
                choice(
                    seq($.kw_function, $.qualified_name),
                    seq($.kw_type, $.qualified_name),
                    seq($.kw_schema, $.qualified_name),
                    $.qualified_name,
                ),

            // ------------------------------------------------------------------
            // SQL statements (function bodies and procedural statements)
            // ------------------------------------------------------------------
            sql_body: ($) =>
                choice($.select_statement, $.insert_statement, $.update_statement, $.delete_statement),

            select_statement: ($) =>
                seq(
                    $.kw_select,
                    optional($.kw_distinct),
                    sep1($.expression, $.comma),
                    optional(
                        seq($.kw_from, field('table', $.qualified_name), optional($.alias)),
                    ),
                    optional(seq($.kw_where, field('condition', $.expression))),
                    optional(seq($.kw_order, $.kw_by, sep1($.expression, $.comma))),
                ),

            insert_statement: ($) =>
                seq(
                    $.kw_insert,
                    $.kw_into,
                    field('table', $.qualified_name),
                    optional($.alias),
                    $.lparen,
                    sep1($._name, $.comma),
                    $.rparen,
                    $.kw_values,
                    $.lparen,
                    sep1($.expression, $.comma),
                    $.rparen,
                    optional(seq($.kw_returning, sep1($.expression, $.comma))),
                ),

            update_statement: ($) =>
                seq(
                    $.kw_update,
                    field('table', $.qualified_name),
                    optional($.alias),
                    $.kw_set,
                    sep1(seq(field('column', $._name), '=', field('value', $.expression)), $.comma),
                    optional(seq($.kw_where, field('condition', $.expression))),
                    optional(seq($.kw_returning, $.expression)),
                ),

            delete_statement: ($) =>
                seq(
                    $.kw_delete,
                    $.kw_from,
                    field('table', $.qualified_name),
                    optional($.alias),
                    optional(seq($.kw_where, field('condition', $.expression))),
                    optional(seq($.kw_returning, $.expression)),
                ),

            alias: ($) =>
                choice(
                    seq($.kw_as, field('name', $.identifier)),
                    field('name', $.identifier),
                ),

            // ------------------------------------------------------------------
            // Procedural statements
            // ------------------------------------------------------------------
            procedural_statement: ($) =>
                choice(
                    $.assignment_statement,
                    $.let_statement,
                    $.call_statement,
                    $.await_statement,
                    $.return_statement,
                    $.if_statement,
                    $.while_statement,
                    $.for_statement,
                    seq($.sql_body, $.semicolon),
                    $.expression_statement,
                ),

            _local_declaration: ($) =>
                choice($.let_declaration, $.const_declaration, $.state_declaration),

            let_declaration: ($) =>
                seq(
                    $.kw_let,
                    field('name', $._name),
                    field('type', $.type_spec),
                    optional(seq($.assignment_operator, field('value', $.expression))),
                    $.semicolon,
                ),

            const_declaration: ($) =>
                seq(
                    $.kw_const,
                    field('name', $._name),
                    field('type', $.type_spec),
                    $.assignment_operator,
                    field('value', $.expression),
                    $.semicolon,
                ),

            state_declaration: ($) =>
                seq(
                    $.kw_state,
                    field('name', $._name),
                    field('type', $.type_spec),
                    optional(
                        seq($.kw_scope, choice($.kw_local, $.kw_session, $.kw_user)),
                    ),
                    optional(seq($.kw_default, field('default', $.expression))),
                    $.semicolon,
                ),

            assignment_statement: ($) =>
                seq(
                    field('target', $.assignable),
                    $.assignment_operator,
                    field('value', $.expression),
                    $.semicolon,
                ),

            let_statement: ($) =>
                seq(
                    $.kw_let,
                    field('name', $._name),
                    optional(field('type', $.type_spec)),
                    $.assignment_operator,
                    field('value', $.expression),
                    $.semicolon,
                ),

            call_statement: ($) =>
                seq(optional($.kw_call), field('callee', $.invocation), $.semicolon),

            await_statement: ($) => seq($.kw_await, $.expression, $.semicolon),

            return_statement: ($) => seq($.kw_return, optional($.expression), $.semicolon),

            if_statement: ($) =>
                seq(
                    $.kw_if,
                    field('condition', $.expression),
                    $.kw_then,
                    repeat($.procedural_statement),
                    repeat(
                        seq(
                            $.kw_elsif,
                            field('condition', $.expression),
                            $.kw_then,
                            repeat($.procedural_statement),
                        ),
                    ),
                    optional(seq($.kw_else, repeat($.procedural_statement))),
                    $.kw_end,
                    $.kw_if,
                    $.semicolon,
                ),

            while_statement: ($) =>
                seq(
                    $.kw_while,
                    field('condition', $.expression),
                    $.kw_loop,
                    repeat($.procedural_statement),
                    $.kw_end,
                    $.kw_loop,
                    $.semicolon,
                ),

            for_statement: ($) =>
                seq(
                    $.kw_for,
                    field('name', $._name),
                    $.kw_in,
                    field('iterable', $.expression),
                    $.kw_loop,
                    repeat($.procedural_statement),
                    $.kw_end,
                    $.kw_loop,
                    $.semicolon,
                ),

            expression_statement: ($) => seq($.expression, $.semicolon),

            assignable: ($) => $.qualified_name,

            // Higher precedence than postfix calls in expressions so a statement
            // like `foo();` parses as a call_statement.
            invocation: ($) =>
                prec(7, seq(field('name', $.qualified_name), $.lparen, optional($.argument_list), $.rparen)),

            // ------------------------------------------------------------------
            // Expressions
            // ------------------------------------------------------------------
            expression: ($) =>
                choice($.logical_expression, $.lambda_expression),

            case_expression: ($) =>
                seq(
                    $.kw_case,
                    optional(field('subject', $.expression)),
                    repeat(
                        seq(
                            $.kw_when,
                            field('condition', $.expression),
                            $.kw_then,
                            field('value', $.expression),
                        ),
                    ),
                    optional(seq($.kw_else, field('else', $.expression))),
                    $.kw_end,
                ),

            lambda_expression: ($) =>
                seq(field('pattern', $.parameter_pattern), $.arrow_operator, field('body', $.expression)),

            parameter_pattern: ($) =>
                choice($._name, seq($.lparen, sep($._name, $.comma), $.rparen)),

            logical_expression: ($) =>
                prec.left(
                    1,
                    choice(
                        seq($.logical_expression, choice($.kw_and, $.kw_or), $.comparison_expression),
                        $.comparison_expression,
                    ),
                ),

            comparison_expression: ($) =>
                prec.left(
                    2,
                    choice(
                        seq(
                            $.comparison_expression,
                            choice($.comparison_operator, $.lt, $.gt),
                            $.additive_expression,
                        ),
                        seq($.comparison_expression, $.kw_is, optional($.kw_not), $.kw_null),
                        seq(
                            $.comparison_expression,
                            $.kw_in,
                            choice(
                                $.additive_expression,
                                seq($.lparen, sep1($.expression, $.comma), $.rparen),
                            ),
                        ),
                        seq($.comparison_expression, choice($.kw_like, $.kw_ilike), $.additive_expression),
                        $.additive_expression,
                    ),
                ),

            additive_expression: ($) =>
                prec.left(
                    3,
                    choice(
                        seq($.additive_expression, $.additive_operator, $.multiplicative_expression),
                        $.multiplicative_expression,
                    ),
                ),

            multiplicative_expression: ($) =>
                prec.left(
                    4,
                    choice(
                        seq($.multiplicative_expression, $.multiplicative_operator, $.unary_expression),
                        $.unary_expression,
                    ),
                ),

            unary_expression: ($) =>
                prec(
                    5,
                    choice(
                        seq(choice($.kw_not, $.unary_operator), $.unary_expression),
                        $.postfix_expression,
                    ),
                ),

            postfix_expression: ($) => seq($.primary_expression, repeat($._postfix_operation)),

            _postfix_operation: ($) =>
                choice(
                    seq($.dot, $._name),
                    seq($.lparen, optional($.argument_list), $.rparen),
                    seq($.lbracket, $.expression, $.rbracket),
                ),

            primary_expression: ($) =>
                choice(
                    $.literal,
                    $.qualified_name,
                    $.list_literal,
                    $.map_literal,
                    $.record_literal,
                    $.case_expression,
                    seq($.lparen, $.expression, $.rparen),
                ),

            argument_list: ($) => sep1($.argument, $.comma),

            argument: ($) =>
                choice(
                    prec(2, seq(field('name', $._name), $.arrow_operator, field('value', $.expression))),
                    prec(1, field('value', $.expression)),
                ),

            list_literal: ($) =>
                seq($.lbracket, sep_trailing($.expression, $.comma), $.rbracket),

            map_literal: ($) => seq($.lbrace, sep_trailing($.map_entry, $.comma), $.rbrace),

            map_entry: ($) =>
                seq(field('key', $.expression), $.colon, field('value', $.expression)),

            record_literal: ($) =>
                prec(8, seq($.qualified_name, $.lbrace, sep_trailing($.record_field, $.comma), $.rbrace)),

            record_field: ($) =>
                seq(field('name', $._name), $.colon, field('value', $.expression)),

            // ------------------------------------------------------------------
            // Types
            // ------------------------------------------------------------------
            type_spec: ($) => prec.right(1, seq($._type_base, optional($.question_mark))),

            _type_base: ($) =>
                choice(
                    $.scalar_type,
                    seq($.kw_ref, $.type_spec),
                    seq($.kw_list, $.lt, $.type_spec, $.gt),
                    seq($.kw_set, $.lt, $.type_spec, $.gt),
                    seq($.kw_map, $.lt, $.type_spec, $.comma, $.type_spec, $.gt),
                    seq($.kw_stream, $.lt, $.type_spec, $.gt),
                    seq($.kw_option, $.lt, $.type_spec, $.gt),
                    seq($.kw_table, $.lparen, sep($.table_column_definition, $.comma), $.rparen),
                    seq($.qualified_name, optional($._generic_args)),
                ),

            _generic_args: ($) => seq($.lt, sep1($.type_spec, $.comma), $.gt),

            scalar_type: ($) =>
                choice(
                    $.kw_boolean,
                    $.kw_bool,
                    $.kw_integer,
                    $.kw_int,
                    $.kw_bigint,
                    $.kw_float,
                    $.kw_decimal,
                    seq($.kw_character, $.kw_large, $.kw_object),
                    $.kw_text,
                    seq($.kw_binary, $.kw_large, $.kw_object),
                    $.kw_bytes,
                    $.kw_uuid,
                    $.kw_date,
                    $.kw_time,
                    $.kw_timestamp,
                    $.kw_duration,
                    $.kw_void,
                ),

            // ------------------------------------------------------------------
            // Names and literals
            // ------------------------------------------------------------------
            // Left-recursive so dotted chains absorb into the qualified name; the
            // dot token carries a higher precedence than the production so the
            // parser continues the chain instead of switching to a postfix member.
            qualified_name: ($) =>
                prec.left(
                    1,
                    choice(
                        seq($.qualified_name, $.dot, $._name),
                        $._name_first,
                    ),
                ),

            // The first component of a name is normally an identifier. Real Orna
            // code additionally uses `security` (schema) and `rows` (variable) as
            // names, and REF(alias) appears as a call; all other keywords are
            // reserved in name-initial position so statement terminators such as
            // END cannot be mistaken for expressions. Later components accept any
            // keyword (std.types.DATE, filter.SET, sys.time.now).
            _name_first: ($) => choice($.identifier, $.kw_ref, $.kw_security, $.kw_rows),

            // Any keyword token, usable as a name component (see _name).
            _keyword: ($) => prec(1, choice(...KEYWORDS.map((word) => $['kw_' + word]))),

            _name: ($) => choice($._keyword, $.identifier),

            _prelude_name: ($) =>
                choice(
                    $.identifier,
                    $.kw_boolean,
                    $.kw_bool,
                    $.kw_integer,
                    $.kw_int,
                    $.kw_bigint,
                    $.kw_float,
                    $.kw_decimal,
                    seq($.kw_character, $.kw_large, $.kw_object),
                    $.kw_text,
                    seq($.kw_binary, $.kw_large, $.kw_object),
                    $.kw_bytes,
                    $.kw_uuid,
                    $.kw_date,
                    $.kw_time,
                    $.kw_timestamp,
                    $.kw_duration,
                    $.kw_void,
                    $.kw_object,
                ),

            identifier: ($) => choice($.quoted_identifier, $._unquoted_identifier),

            _unquoted_identifier: ($) => /[a-zA-Z_][a-zA-Z0-9_]*/,

            quoted_identifier: ($) => token(seq('"', repeat(choice(/[^"]/, '""')), '"')),

            literal: ($) =>
                choice(
                    $.string_literal,
                    $.number,
                    $.boolean_literal,
                    $.null_literal,
                    $.date_literal,
                    $.timestamp_literal,
                    $.bytes_literal,
                ),

            string_literal: ($) => token(seq("'", repeat(choice(/[^']/, "''")), "'")),

            number: ($) => token(/\d+(\.\d+)?/),

            // Higher precedence than _keyword so NULL/TRUE/FALSE in expression
            // position parse as literals, not as keyword-shaped names.
            boolean_literal: ($) => prec(2, choice($.kw_true, $.kw_false)),

            null_literal: ($) => prec(2, $.kw_null),

            date_literal: ($) => seq($.kw_date, $.string_literal),

            timestamp_literal: ($) => seq($.kw_timestamp, $.string_literal),

            bytes_literal: ($) => seq($.kw_bytes, $.string_literal),

            // ------------------------------------------------------------------
            // Comments and punctuation
            // ------------------------------------------------------------------
            comment: ($) =>
                token(
                    choice(
                        seq('--', /[^\n]*/),
                        seq('/*', /[^*]*\*+([^/*][^*]*\*+)*/, '/'),
                    ),
                ),

            lparen: ($) => token('('),
            rparen: ($) => token(')'),
            lbracket: ($) => token('['),
            rbracket: ($) => token(']'),
            lbrace: ($) => token('{'),
            rbrace: ($) => token('}'),
            comma: ($) => token(','),
            semicolon: ($) => token(';'),
            dot: ($) => token(prec(3, '.')),
            colon: ($) => token(':'),
            question_mark: ($) => token('?'),
            lt: ($) => token('<'),
            gt: ($) => token('>'),

            comparison_operator: ($) => token(choice('=', '<>', '!=', '<=', '>=')),
            additive_operator: ($) => token(choice('+', '-', '||')),
            multiplicative_operator: ($) => token(choice('*', '/', '%')),
            unary_operator: ($) => token(choice('+', '-')),
            assignment_operator: ($) => token(':='),
            arrow_operator: ($) => token('=>'),
        },
        keywordRules,
    ),
});
