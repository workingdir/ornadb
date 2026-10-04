//! Syntax-aware LSP folding and selection ranges.

use lsp_types::{FoldingRange, FoldingRangeKind, SelectionRange};
use orna_syntax_v1::{
    AssignmentTarget, Declaration, Expr, FieldInitializer, FunctionSignature, ImplMember, Item,
    Parameter, Pattern, ProtocolMember, RecordField, Statement, StringSegment,
    SyntaxSpan as SourceSpan, SyntaxTree, TableMember, TypeExpr, TypeMember, TypeRepresentation,
    UnitDefinition, UseTail, Visibility, lex,
};

use crate::documents::PositionMapper;

pub(crate) fn selection_ranges(
    tree: &SyntaxTree,
    text: &str,
    positions: &[lsp_types::Position],
    mapper: &PositionMapper<'_>,
) -> Vec<SelectionRange> {
    let mut spans = vec![SourceSpan::new(0, text.len())];
    collect_tree_spans(tree, &mut spans);
    if let Ok(tokens) = lex(text) {
        spans.extend(
            tokens
                .into_iter()
                .filter(|token| token.span.start < token.span.end)
                .map(|token| token.span),
        );
    }
    spans.extend(scan_comments(text).into_iter().map(|comment| comment.span));
    spans.retain(|span| span.start <= span.end && span.end <= text.len());
    spans.sort_by_key(|span| (span.end.saturating_sub(span.start), span.start, span.end));
    spans.dedup_by_key(|span| (span.start, span.end));
    let line_spans = source_line_spans(text);

    positions
        .iter()
        .copied()
        .map(|position| {
            let byte = mapper.byte_offset(position);
            let mut chain = Vec::new();
            for span in spans
                .iter()
                .filter(|span| contains_position(span, byte, text.len()))
            {
                if chain.last().is_none_or(|inner: &SourceSpan| {
                    span.start <= inner.start
                        && span.end >= inner.end
                        && (span.start < inner.start || span.end > inner.end)
                }) {
                    chain.push(span.clone());
                }
            }
            // Trivia-only positions still need a useful selectable range.
            // Add a line range only when the syntax tree has no node at the
            // cursor, so it cannot interrupt a semantic parent chain.
            if chain.len() <= 1 {
                let line_span = line_spans
                    .iter()
                    .find(|span| {
                        contains_position(span, byte, text.len())
                            || (span.start == span.end && span.start == byte)
                    })
                    .cloned();
                if let Some(line_span) = line_span
                    && !spans
                        .iter()
                        .any(|span| span.start == line_span.start && span.end == line_span.end)
                {
                    chain.insert(0, line_span);
                }
            }
            if chain.is_empty() {
                chain.push(SourceSpan::new(0, text.len()));
            }
            let mut selection = None;
            for span in chain.into_iter().rev() {
                selection = Some(SelectionRange {
                    range: mapper.range(&span),
                    parent: selection.map(Box::new),
                });
            }
            selection.expect("the source range is always available")
        })
        .collect()
}

pub(crate) fn folding_ranges(
    tree: &SyntaxTree,
    text: &str,
    mapper: &PositionMapper<'_>,
) -> Vec<FoldingRange> {
    let mut spans = Vec::new();
    collect_tree_spans(tree, &mut spans);
    let mut ranges = spans
        .into_iter()
        .filter_map(|span| folding_range(&span, text.len(), mapper, None))
        .collect::<Vec<_>>();

    for span in import_group_spans(tree, text) {
        if let Some(range) =
            folding_range(&span, text.len(), mapper, Some(FoldingRangeKind::Imports))
        {
            ranges.push(range);
        }
    }
    for comment in comment_groups(text) {
        if let Some(range) = folding_range(
            &comment,
            text.len(),
            mapper,
            Some(FoldingRangeKind::Comment),
        ) {
            ranges.push(range);
        }
    }

    ranges.sort_by_key(|range| {
        (
            range.start_line,
            range.start_character.unwrap_or_default(),
            range.end_line,
            range.end_character.unwrap_or_default(),
            folding_kind_order(range.kind.as_ref()),
        )
    });
    ranges.dedup_by(|later, earlier| {
        later.start_line == earlier.start_line
            && later.start_character == earlier.start_character
            && later.end_line == earlier.end_line
            && later.end_character == earlier.end_character
    });
    ranges
}

fn contains_position(span: &SourceSpan, byte: usize, text_len: usize) -> bool {
    span.start <= byte && (byte < span.end || (byte == text_len && span.end == text_len))
}

fn source_line_spans(text: &str) -> Vec<SourceSpan> {
    let mut spans = Vec::new();
    let mut start = 0usize;
    for line in text.split_inclusive('\n') {
        let mut end = start + line.len();
        if line.ends_with('\n') {
            end -= 1;
        }
        if end > start && text.as_bytes().get(end - 1) == Some(&b'\r') {
            end -= 1;
        }
        spans.push(SourceSpan::new(start, end));
        start += line.len();
    }
    if text.is_empty() || !text.ends_with('\n') {
        spans.push(SourceSpan::new(start.min(text.len()), text.len()));
    } else {
        spans.push(SourceSpan::new(text.len(), text.len()));
    }
    spans
}

fn collect_tree_spans(tree: &SyntaxTree, spans: &mut Vec<SourceSpan>) {
    for item in &tree.items {
        collect_item_spans(item, spans);
    }
}

fn push_span(span: &SourceSpan, spans: &mut Vec<SourceSpan>) {
    if span.start <= span.end {
        spans.push(span.clone());
    }
}

fn collect_item_spans(item: &Item, spans: &mut Vec<SourceSpan>) {
    push_span(&item.span, spans);
    if let Visibility::Public { span } = &item.visibility {
        push_span(span, spans);
    }
    match &item.declaration {
        Declaration::Use { path, tail } => {
            for segment in path {
                push_span(&segment.span, spans);
            }
            match tail {
                UseTail::Alias { span, .. } | UseTail::Glob { span } => push_span(span, spans),
                UseTail::Names(names) => {
                    for name in names {
                        push_span(&name.span, spans);
                    }
                }
                UseTail::None => {}
            }
        }
        Declaration::Table { keys, members, .. } => {
            for key in keys {
                collect_parameter_spans(key, spans);
            }
            for member in members {
                match member {
                    TableMember::Field {
                        ty,
                        initializer,
                        span,
                        ..
                    } => {
                        push_span(span, spans);
                        collect_type_spans(ty, spans);
                        if let Some(
                            FieldInitializer::Default(value) | FieldInitializer::Computed(value),
                        ) = initializer
                        {
                            collect_expr_spans(value, spans);
                        }
                    }
                    TableMember::Assertion { value, span } => {
                        push_span(span, spans);
                        collect_expr_spans(value, spans);
                    }
                    TableMember::Implementation {
                        implementation,
                        span,
                    } => {
                        push_span(span, spans);
                        collect_implementation_spans(implementation, spans);
                    }
                }
            }
        }
        Declaration::Function { signature, body } => {
            collect_signature_spans(signature, spans);
            collect_expr_spans(body, spans);
        }
        Declaration::Protocol {
            generics, members, ..
        } => {
            for generic in generics {
                push_span(&generic.span, spans);
                for bound in &generic.bounds {
                    collect_type_spans(bound, spans);
                }
            }
            for member in members {
                match member {
                    ProtocolMember::Function { signature, span } => {
                        push_span(span, spans);
                        collect_signature_spans(signature, spans);
                    }
                    ProtocolMember::Static { ty, span, .. } => {
                        push_span(span, spans);
                        collect_type_spans(ty, spans);
                    }
                }
            }
        }
        Declaration::Enum {
            generics, variants, ..
        } => {
            for generic in generics {
                push_span(&generic.span, spans);
                for bound in &generic.bounds {
                    collect_type_spans(bound, spans);
                }
            }
            for variant in variants {
                push_span(&variant.span, spans);
                for field in &variant.fields {
                    push_span(&field.span, spans);
                    collect_type_spans(&field.ty, spans);
                }
            }
        }
        Declaration::Dimension { expression, .. } => {
            if let Some(expression) = expression {
                push_span(&expression.span, spans);
                for (_, ty, _, span) in &expression.terms {
                    push_span(span, spans);
                    collect_type_spans(ty, spans);
                }
            }
        }
        Declaration::Unit {
            dimension,
            definition,
            ..
        } => {
            collect_type_spans(dimension, spans);
            if let UnitDefinition::Derived { value, offset, .. } = definition {
                collect_expr_spans(value, spans);
                if let Some(offset) = offset {
                    collect_expr_spans(offset, spans);
                }
            }
        }
        Declaration::Type {
            generics,
            representation,
            ..
        } => {
            for generic in generics {
                push_span(&generic.span, spans);
                for bound in &generic.bounds {
                    collect_type_spans(bound, spans);
                }
            }
            collect_type_representation_spans(representation, spans);
        }
        Declaration::Assertion { value } => collect_expr_spans(value, spans),
        Declaration::Let {
            pattern,
            annotation,
            value,
        } => {
            collect_pattern_spans(pattern, spans);
            if let Some(annotation) = annotation {
                collect_type_spans(annotation, spans);
            }
            collect_expr_spans(value, spans);
        }
    }
}

fn collect_signature_spans(signature: &FunctionSignature, spans: &mut Vec<SourceSpan>) {
    push_span(&signature.span, spans);
    for generic in &signature.generics {
        push_span(&generic.span, spans);
        for bound in &generic.bounds {
            collect_type_spans(bound, spans);
        }
    }
    for parameter in &signature.parameters {
        collect_parameter_spans(parameter, spans);
    }
    if let Some(result) = &signature.result {
        collect_type_spans(result, spans);
    }
}

fn collect_parameter_spans(parameter: &Parameter, spans: &mut Vec<SourceSpan>) {
    push_span(&parameter.span, spans);
    collect_pattern_spans(&parameter.pattern, spans);
    if let Some(annotation) = &parameter.annotation {
        collect_type_spans(annotation, spans);
    }
    if let Some(default) = &parameter.default {
        collect_expr_spans(default, spans);
    }
}

fn collect_type_spans(ty: &TypeExpr, spans: &mut Vec<SourceSpan>) {
    match ty {
        TypeExpr::Name {
            arguments, span, ..
        } => {
            push_span(span, spans);
            for argument in arguments {
                collect_type_spans(argument, spans);
            }
        }
        TypeExpr::Optional { inner, span } | TypeExpr::List { inner, span } => {
            push_span(span, spans);
            collect_type_spans(inner, spans);
        }
        TypeExpr::Product { lhs, rhs, span, .. } => {
            push_span(span, spans);
            collect_type_spans(lhs, spans);
            collect_type_spans(rhs, spans);
        }
        TypeExpr::Record { fields, span } => {
            push_span(span, spans);
            for (_, ty, field_span) in fields {
                push_span(field_span, spans);
                collect_type_spans(ty, spans);
            }
        }
        TypeExpr::Tuple { elements, span } => {
            push_span(span, spans);
            for element in elements {
                collect_type_spans(element, spans);
            }
        }
        TypeExpr::Function {
            parameters,
            result,
            span,
        } => {
            push_span(span, spans);
            for parameter in parameters {
                collect_type_spans(parameter, spans);
            }
            collect_type_spans(result, spans);
        }
    }
}

fn collect_pattern_spans(pattern: &Pattern, spans: &mut Vec<SourceSpan>) {
    match pattern {
        Pattern::Name(_, span)
        | Pattern::Wildcard(span)
        | Pattern::Literal { span, .. }
        | Pattern::Tuple { span, .. }
        | Pattern::List { span, .. }
        | Pattern::Record { span, .. }
        | Pattern::Constructor { span, .. } => push_span(span, spans),
    }
    match pattern {
        Pattern::Tuple { elements, .. } | Pattern::List { elements, .. } => {
            for element in elements {
                collect_pattern_spans(element, spans);
            }
        }
        Pattern::Record { fields, .. } => {
            for (_, pattern, span) in fields {
                push_span(span, spans);
                if let Some(pattern) = pattern {
                    collect_pattern_spans(pattern, spans);
                }
            }
        }
        Pattern::Constructor {
            path,
            arguments,
            fields,
            ..
        } => {
            for segment in path {
                push_span(&segment.span, spans);
            }
            for argument in arguments {
                collect_pattern_spans(argument, spans);
            }
            for field in fields {
                push_span(&field.span, spans);
                if let Some(pattern) = &field.pattern {
                    collect_pattern_spans(pattern, spans);
                }
            }
        }
        Pattern::Name(_, _) | Pattern::Wildcard(_) | Pattern::Literal { .. } => {}
    }
}

fn collect_type_representation_spans(
    representation: &TypeRepresentation,
    spans: &mut Vec<SourceSpan>,
) {
    match representation {
        TypeRepresentation::Alias { ty, refinements } => {
            collect_type_spans(ty, spans);
            for member in refinements {
                collect_type_member_spans(member, spans);
            }
        }
        TypeRepresentation::Nominal { members } => {
            for member in members {
                collect_type_member_spans(member, spans);
            }
        }
    }
}

fn collect_type_member_spans(member: &TypeMember, spans: &mut Vec<SourceSpan>) {
    match member {
        TypeMember::Field {
            ty,
            initializer,
            span,
            ..
        } => {
            push_span(span, spans);
            collect_type_spans(ty, spans);
            if let Some(initializer) = initializer {
                collect_expr_spans(initializer, spans);
            }
        }
        TypeMember::Assertion { value, span } => {
            push_span(span, spans);
            collect_expr_spans(value, spans);
        }
        TypeMember::Implementation {
            implementation,
            span,
        } => {
            push_span(span, spans);
            collect_implementation_spans(implementation, spans);
        }
    }
}

fn collect_implementation_spans(
    implementation: &orna_syntax_v1::Implementation,
    spans: &mut Vec<SourceSpan>,
) {
    push_span(&implementation.span, spans);
    collect_type_spans(&implementation.protocol, spans);
    for member in &implementation.members {
        match member {
            ImplMember::Function {
                signature,
                body,
                span,
            } => {
                push_span(span, spans);
                collect_signature_spans(signature, spans);
                collect_expr_spans(body, spans);
            }
            ImplMember::Static {
                ty, value, span, ..
            } => {
                push_span(span, spans);
                if let Some(ty) = ty {
                    collect_type_spans(ty, spans);
                }
                collect_expr_spans(value, spans);
            }
        }
    }
}

fn collect_expr_spans(expression: &Expr, spans: &mut Vec<SourceSpan>) {
    push_span(&expression.span(), spans);
    match expression {
        Expr::InterpolatedString { segments, .. } => {
            for segment in segments {
                match segment {
                    StringSegment::Text { span, .. } => push_span(span, spans),
                    StringSegment::Expression { value, span } => {
                        push_span(span, spans);
                        collect_expr_spans(value, spans);
                    }
                }
            }
        }
        Expr::Unary { rhs, .. } | Expr::Group { inner: rhs, .. } => {
            collect_expr_spans(rhs, spans);
        }
        Expr::Binary { lhs, rhs, .. } => {
            collect_expr_spans(lhs, spans);
            collect_expr_spans(rhs, spans);
        }
        Expr::Range { lower, upper, .. } => {
            if let Some(lower) = lower {
                collect_expr_spans(lower, spans);
            }
            if let Some(upper) = upper {
                collect_expr_spans(upper, spans);
            }
        }
        Expr::Call {
            callee, arguments, ..
        } => {
            collect_expr_spans(callee, spans);
            for argument in arguments {
                push_span(&argument.span, spans);
                collect_expr_spans(&argument.value, spans);
            }
        }
        Expr::GenericCall {
            callee,
            type_arguments,
            arguments,
            ..
        } => {
            collect_expr_spans(callee, spans);
            for ty in type_arguments {
                collect_type_spans(ty, spans);
            }
            for argument in arguments {
                push_span(&argument.span, spans);
                collect_expr_spans(&argument.value, spans);
            }
        }
        Expr::Index { base, index, .. } => {
            collect_expr_spans(base, spans);
            collect_expr_spans(index, spans);
        }
        Expr::Field { base, name, span } => {
            push_span(
                &SourceSpan::new(span.end.saturating_sub(name.len()), span.end),
                spans,
            );
            collect_expr_spans(base, spans);
        }
        Expr::Tuple { elements, .. } | Expr::List { elements, .. } => {
            for element in elements {
                collect_expr_spans(element, spans);
            }
        }
        Expr::Record { fields, .. } | Expr::Nominal { fields, .. } => {
            if let Expr::Nominal { path, .. } = expression {
                for segment in path {
                    push_span(&segment.span, spans);
                }
            }
            for field in fields {
                collect_record_field_spans(field, spans);
            }
        }
        Expr::Lambda {
            parameters, body, ..
        } => {
            for parameter in parameters {
                push_span(&parameter.span, spans);
                collect_pattern_spans(&parameter.pattern, spans);
                if let Some(annotation) = &parameter.annotation {
                    collect_type_spans(annotation, spans);
                }
            }
            collect_expr_spans(body, spans);
        }
        Expr::Block {
            statements, tail, ..
        } => {
            for statement in statements {
                collect_statement_spans(statement, spans);
            }
            if let Some(tail) = tail {
                collect_expr_spans(tail, spans);
            }
        }
        Expr::Control {
            binding,
            condition,
            body,
            arms,
            alternate,
            ..
        } => {
            if let Some(binding) = binding {
                collect_pattern_spans(binding, spans);
            }
            if let Some(condition) = condition {
                collect_expr_spans(condition, spans);
            }
            if let Some(body) = body {
                collect_expr_spans(body, spans);
            }
            for arm in arms {
                push_span(&arm.span, spans);
                collect_pattern_spans(&arm.pattern, spans);
                if let Some(guard) = &arm.guard {
                    collect_expr_spans(guard, spans);
                }
                collect_expr_spans(&arm.body, spans);
            }
            if let Some(alternate) = alternate {
                collect_expr_spans(alternate, spans);
            }
        }
        Expr::Name { .. } | Expr::Literal { .. } | Expr::ReplBinding { .. } => {}
    }
}

fn collect_record_field_spans(field: &RecordField, spans: &mut Vec<SourceSpan>) {
    push_span(&field.span, spans);
    collect_expr_spans(&field.value, spans);
}

fn collect_statement_spans(statement: &Statement, spans: &mut Vec<SourceSpan>) {
    match statement {
        Statement::Let {
            pattern,
            annotation,
            value,
            span,
        } => {
            push_span(span, spans);
            collect_pattern_spans(pattern, spans);
            if let Some(annotation) = annotation {
                collect_type_spans(annotation, spans);
            }
            collect_expr_spans(value, spans);
        }
        Statement::Assert { value, span }
        | Statement::Expression { value, span }
        | Statement::Control { value, span } => {
            push_span(span, spans);
            collect_expr_spans(value, spans);
        }
        Statement::Return { value, span } | Statement::Break { value, span } => {
            push_span(span, spans);
            if let Some(value) = value {
                collect_expr_spans(value, spans);
            }
        }
        Statement::Continue { span } => push_span(span, spans),
        Statement::Assignment {
            target,
            value,
            span,
            ..
        } => {
            push_span(span, spans);
            collect_assignment_target_spans(target, spans);
            collect_expr_spans(value, spans);
        }
    }
}

fn collect_assignment_target_spans(target: &AssignmentTarget, spans: &mut Vec<SourceSpan>) {
    match target {
        AssignmentTarget::Name { span, .. } => push_span(span, spans),
        AssignmentTarget::Field { base, span, name } => {
            push_span(span, spans);
            push_span(
                &SourceSpan::new(span.end.saturating_sub(name.len()), span.end),
                spans,
            );
            collect_assignment_target_spans(base, spans);
        }
        AssignmentTarget::Index { base, index, span } => {
            push_span(span, spans);
            collect_assignment_target_spans(base, spans);
            collect_expr_spans(index, spans);
        }
    }
}

fn import_group_spans(tree: &SyntaxTree, text: &str) -> Vec<SourceSpan> {
    let imports = tree
        .items
        .iter()
        .filter_map(|item| {
            matches!(item.declaration, Declaration::Use { .. }).then_some(&item.span)
        })
        .collect::<Vec<_>>();
    let mut groups = Vec::new();
    let mut first = None;
    let mut last = None;
    let mut count = 0usize;
    let flush = |first: &mut Option<usize>,
                 last: &mut Option<usize>,
                 count: &mut usize,
                 groups: &mut Vec<SourceSpan>| {
        if let (Some(start), Some(end)) = (*first, *last)
            && *count > 1
        {
            groups.push(SourceSpan::new(start, end));
        }
        *first = None;
        *last = None;
        *count = 0;
    };
    for span in imports {
        if let Some(previous_end) = last {
            let gap = text.get(previous_end..span.start).unwrap_or_default();
            if !gap.chars().all(char::is_whitespace) || gap.matches('\n').count() > 1 {
                flush(&mut first, &mut last, &mut count, &mut groups);
            }
        }
        first.get_or_insert(span.start);
        last = Some(span.end);
        count += 1;
    }
    flush(&mut first, &mut last, &mut count, &mut groups);
    groups
}

fn folding_range(
    span: &SourceSpan,
    text_len: usize,
    mapper: &PositionMapper<'_>,
    kind: Option<FoldingRangeKind>,
) -> Option<FoldingRange> {
    if span.start >= span.end || span.end > text_len {
        return None;
    }
    let start = mapper.position(span.start);
    let end = mapper.position(span.end);
    (start.line < end.line).then_some(FoldingRange {
        start_line: start.line,
        start_character: Some(start.character),
        end_line: end.line,
        end_character: Some(end.character),
        kind,
        collapsed_text: None,
    })
}

fn folding_kind_order(kind: Option<&FoldingRangeKind>) -> u8 {
    match kind {
        Some(FoldingRangeKind::Comment) => 0,
        Some(FoldingRangeKind::Imports) => 1,
        Some(FoldingRangeKind::Region) => 2,
        None => 3,
    }
}

#[derive(Clone)]
struct CommentSpan {
    span: SourceSpan,
    line_comment: bool,
}

fn comment_groups(text: &str) -> Vec<SourceSpan> {
    let comments = scan_comments(text);
    let mapper = PositionMapper::new(text);
    let mut groups = Vec::new();
    let mut line_group: Option<(usize, usize, usize, usize)> = None;
    for comment in comments {
        if comment.line_comment {
            let start_line = mapper.position(comment.span.start).line as usize;
            if let Some((start, previous_end, group_start_line, previous_line)) = line_group {
                if start_line == previous_line + 1 {
                    line_group = Some((start, comment.span.end, group_start_line, start_line));
                    continue;
                }
                if previous_line > group_start_line {
                    groups.push(SourceSpan::new(start, previous_end));
                }
                line_group = Some((comment.span.start, comment.span.end, start_line, start_line));
            } else {
                line_group = Some((comment.span.start, comment.span.end, start_line, start_line));
            }
        } else {
            if let Some((start, end, start_line, last_line)) = line_group.take()
                && last_line > start_line
            {
                groups.push(SourceSpan::new(start, end));
            }
            groups.push(comment.span);
        }
    }
    if let Some((start, end, start_line, last_line)) = line_group
        && last_line > start_line
    {
        groups.push(SourceSpan::new(start, end));
    }
    groups
}

fn scan_comments(text: &str) -> Vec<CommentSpan> {
    let bytes = text.as_bytes();
    let mut comments = Vec::new();
    let mut at = 0usize;
    while at < bytes.len() {
        if bytes[at] == b'"' {
            at += 1;
            while at < bytes.len() {
                match bytes[at] {
                    b'\\' => {
                        at += 1;
                        if at < bytes.len() {
                            at += text[at..].chars().next().map_or(1, char::len_utf8);
                        }
                    }
                    b'"' => {
                        at += 1;
                        break;
                    }
                    _ => at += text[at..].chars().next().map_or(1, char::len_utf8),
                }
            }
        } else if bytes.get(at..at + 2) == Some(b"//") {
            let start = at;
            at += 2;
            while at < bytes.len() && bytes[at] != b'\n' {
                at += text[at..].chars().next().map_or(1, char::len_utf8);
            }
            comments.push(CommentSpan {
                span: SourceSpan::new(start, at),
                line_comment: true,
            });
        } else if bytes.get(at..at + 2) == Some(b"/*") {
            let start = at;
            at += 2;
            let mut depth = 1usize;
            while at < bytes.len() && depth > 0 {
                if bytes.get(at..at + 2) == Some(b"/*") {
                    depth += 1;
                    at += 2;
                } else if bytes.get(at..at + 2) == Some(b"*/") {
                    depth -= 1;
                    at += 2;
                } else {
                    at += text[at..].chars().next().map_or(1, char::len_utf8);
                }
            }
            comments.push(CommentSpan {
                span: SourceSpan::new(start, at),
                line_comment: false,
            });
        } else {
            at += text[at..].chars().next().map_or(1, char::len_utf8);
        }
    }
    comments
}
