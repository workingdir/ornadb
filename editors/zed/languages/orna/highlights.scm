; Orna syntax highlighting queries.
; Standard capture names so Neovim, Helix and Zed consume them directly.

; Comments and literals
(comment) @comment
(string_literal) @string
(number) @number
(client_integer_literal) @number
(date_literal) @constant
(timestamp_literal) @constant
(bytes_literal) @string
[(boolean_literal) (null_literal)] @constant.builtin

; Keywords
[
    (kw_create) (kw_schema) (kw_type) (kw_as) (kw_enum) (kw_object)
    (kw_value) (kw_opaque) (kw_primitive) (kw_final) (kw_documentation)
    (kw_immutable) (kw_transient) (kw_persistable) (kw_sealed) (kw_kernel)
    (kw_contract) (kw_export) (kw_to) (kw_prelude) (kw_server) (kw_function)
    (kw_client) (kw_external) (kw_runtime) (kw_returns) (kw_table) (kw_rows) (kw_security)
    (kw_invoker) (kw_definer) (kw_transaction) (kw_atomic) (kw_read) (kw_only)
    (kw_manual) (kw_volatility) (kw_stable) (kw_volatile) (kw_requires)
    (kw_capability) (kw_alter) (kw_rename) (kw_field) (kw_add) (kw_drop)
    (kw_cascade) (kw_restrict) (kw_user) (kw_disabled) (kw_role) (kw_grant)
    (kw_revoke) (kw_on) (kw_from) (kw_execute) (kw_select) (kw_insert)
    (kw_update) (kw_delete) (kw_inspect) (kw_is) (kw_begin) (kw_end) (kw_if)
    (kw_then) (kw_elsif) (kw_else) (kw_while) (kw_loop) (kw_for) (kw_in)
    (kw_let) (kw_const) (kw_state) (kw_scope) (kw_local) (kw_session)
    (kw_default) (kw_not) (kw_null) (kw_unique) (kw_check) (kw_call)
    (kw_await) (kw_return) (kw_and) (kw_or) (kw_like) (kw_ilike) (kw_true)
    (kw_false) (kw_case) (kw_when) (kw_ref) (kw_list) (kw_set) (kw_map)
    (kw_stream) (kw_option) (kw_boolean) (kw_bool) (kw_integer) (kw_int)
    (kw_bigint) (kw_float) (kw_decimal) (kw_character) (kw_large) (kw_text)
    (kw_binary) (kw_bytes) (kw_uuid) (kw_date) (kw_time)
    (kw_timestamp) (kw_duration) (kw_void) (kw_distinct) (kw_order) (kw_by)
    (kw_values) (kw_returning) (kw_into) (kw_where)
] @keyword

; Operators
[
    (comparison_operator) (additive_operator) (multiplicative_operator)
    (unary_operator) (assignment_operator) (arrow_operator)
] @operator

; Punctuation
[
    (lparen) (rparen) (lbracket) (rbracket) (lbrace) (rbrace) (comma)
    (semicolon) (dot) (colon) (question_mark)
] @punctuation

; Qualified names: capitalized names are types, everything else is a
; namespace. Statement names override the general rules below.
(qualified_name) @namespace
((qualified_name) @type (#match? @type "^[A-Z]"))

(create_schema_statement name: (qualified_name) @namespace)
(create_enum_type_statement name: (qualified_name) @type)
(create_object_type_statement name: (qualified_name) @type)
(create_value_type_statement name: (qualified_name) @type)
(export_type_statement name: (qualified_name) @type)
(create_server_function_statement name: (qualified_name) @function)
(create_client_function_statement name: (qualified_name) @function)
(create_external_client_function_statement name: (qualified_name) @function)
(select_statement table: (qualified_name) @type)
(insert_statement table: (qualified_name) @type)
(update_statement table: (qualified_name) @type)
(delete_statement table: (qualified_name) @type)
(call_statement callee: (invocation name: (qualified_name) @function))
(client_call_expression callee: (client_call_callee) @function)

; Names and fields
(parameter_definition name: (identifier) @parameter)
(field_definition name: (identifier) @property)
(table_column_definition name: (identifier) @property)
(record_field name: (identifier) @property)
(update_statement column: (identifier) @property)
(alter_type_statement
    old_name: (_) @property
    new_name: (_) @property)

(let_declaration name: (identifier) @variable)
(let_statement name: (identifier) @variable)
(const_declaration name: (identifier) @variable)
(state_declaration name: (identifier) @variable)
(client_local_declaration name: (_) @variable)
(client_let_statement name: (_) @variable)
(client_assignment_statement target: (_) @variable)
(client_state_declaration name: (_) @variable)
(for_statement name: (identifier) @variable)
(create_user_statement name: (identifier) @variable)
(create_role_statement name: (identifier) @variable)

; CLIENT parameter and field reads
(client_parameter_read) @parameter
(client_field_path parameter: (_) @parameter)
(client_field_path field: (_) @property)
