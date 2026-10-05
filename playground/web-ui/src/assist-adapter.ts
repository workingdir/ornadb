export interface CompletionRankingFields {
  sortText?: string;
  preselect?: boolean;
}

export interface SignatureHelpParameterFields {
  label: string | [number, number];
  documentation?: string;
}

export interface SignatureHelpSignatureFields {
  label: string;
  parameters: SignatureHelpParameterFields[];
  documentation?: string;
}

export interface SignatureHelpFields {
  signatures: SignatureHelpSignatureFields[];
  activeSignature: number;
  activeParameter: number;
}

export interface InlayHintLabelPartFields {
  label: string;
  tooltip?: string;
}

export interface InlayHintFields {
  position: { line: number; character: number };
  label: string | InlayHintLabelPartFields[];
  kind?: 1 | 2;
  tooltip?: string;
  paddingLeft?: boolean;
  paddingRight?: boolean;
}

export interface MonacoSymbolRangeFields {
  startLineNumber: number;
  startColumn: number;
  endLineNumber: number;
  endColumn: number;
}

export interface DocumentSymbolFields {
  name: string;
  detail: string;
  kind: number;
  tags: number[];
  range: MonacoSymbolRangeFields;
  selectionRange: MonacoSymbolRangeFields;
  children: DocumentSymbolFields[];
}

export function completionRankingFields(value: Record<string, unknown>): CompletionRankingFields {
  const fields: CompletionRankingFields = {};
  if (typeof value.sortText === 'string') fields.sortText = value.sortText;
  if (typeof value.preselect === 'boolean') fields.preselect = value.preselect;
  return fields;
}

export function hoverMarkdown(value: unknown): string {
  if (typeof value === 'string') return value;
  if (Array.isArray(value)) return value.map(hoverMarkdown).filter(Boolean).join('\n\n');
  if (typeof value !== 'object' || value === null) return '';
  const record = value as Record<string, unknown>;
  if (typeof record.value !== 'string') return '';
  if (typeof record.language === 'string') {
    return `\`\`\`${record.language}\n${record.value}\n\`\`\``;
  }
  return record.value;
}

export function signatureHelpFields(value: unknown): SignatureHelpFields | undefined {
  if (typeof value !== 'object' || value === null || Array.isArray(value)) return undefined;
  const response = value as Record<string, unknown>;
  if (!Array.isArray(response.signatures)) return undefined;

  const signatures = response.signatures.flatMap((entry): SignatureHelpSignatureFields[] => {
    if (typeof entry !== 'object' || entry === null || Array.isArray(entry)) return [];
    const signature = entry as Record<string, unknown>;
    if (typeof signature.label !== 'string') return [];
    const parameters = Array.isArray(signature.parameters)
      ? signature.parameters.flatMap((parameterEntry): SignatureHelpParameterFields[] => {
        if (typeof parameterEntry !== 'object' || parameterEntry === null || Array.isArray(parameterEntry)) return [];
        const parameter = parameterEntry as Record<string, unknown>;
        const label = parameter.label;
        const validLabel = typeof label === 'string'
          || (Array.isArray(label) && label.length === 2 && label.every(Number.isInteger));
        if (!validLabel) return [];
        const mapped: SignatureHelpParameterFields = { label: label as string | [number, number] };
        if (parameter.documentation) mapped.documentation = hoverMarkdown(parameter.documentation);
        return [mapped];
      })
      : [];
    const mapped: SignatureHelpSignatureFields = { label: signature.label, parameters };
    if (signature.documentation) mapped.documentation = hoverMarkdown(signature.documentation);
    return [mapped];
  });
  if (signatures.length === 0) return undefined;

  return {
    signatures,
    activeSignature: typeof response.activeSignature === 'number' ? response.activeSignature : 0,
    activeParameter: typeof response.activeParameter === 'number' ? response.activeParameter : 0,
  };
}

export function inlayHintFields(value: unknown): InlayHintFields[] {
  if (!Array.isArray(value)) return [];
  return value.flatMap((entry): InlayHintFields[] => {
    if (typeof entry !== 'object' || entry === null || Array.isArray(entry)) return [];
    const hint = entry as Record<string, unknown>;
    const position = hint.position;
    if (typeof position !== 'object' || position === null || Array.isArray(position)) return [];
    const coordinates = position as Record<string, unknown>;
    if (!Number.isInteger(coordinates.line) || Number(coordinates.line) < 0
      || !Number.isInteger(coordinates.character) || Number(coordinates.character) < 0) return [];
    let label: InlayHintFields['label'];
    if (typeof hint.label === 'string') {
      label = hint.label;
    } else if (Array.isArray(hint.label)) {
      const parts = hint.label.flatMap((partEntry): InlayHintLabelPartFields[] => {
        if (typeof partEntry !== 'object' || partEntry === null || Array.isArray(partEntry)) return [];
        const part = partEntry as Record<string, unknown>;
        if (typeof part.value !== 'string') return [];
        const mapped: InlayHintLabelPartFields = { label: part.value };
        if (part.tooltip) mapped.tooltip = hoverMarkdown(part.tooltip);
        return [mapped];
      });
      if (parts.length === 0) return [];
      label = parts;
    } else {
      return [];
    }

    const mapped: InlayHintFields = {
      position: { line: Number(coordinates.line), character: Number(coordinates.character) },
      label,
    };
    if (hint.kind === 1 || hint.kind === 2) mapped.kind = hint.kind;
    if (hint.tooltip) mapped.tooltip = hoverMarkdown(hint.tooltip);
    if (typeof hint.paddingLeft === 'boolean') mapped.paddingLeft = hint.paddingLeft;
    if (typeof hint.paddingRight === 'boolean') mapped.paddingRight = hint.paddingRight;
    return [mapped];
  });
}

function record(value: unknown): Record<string, unknown> | undefined {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
    ? value as Record<string, unknown>
    : undefined;
}

function symbolRangeFields(value: unknown): MonacoSymbolRangeFields | undefined {
  const range = record(value);
  const start = record(range?.start);
  const end = record(range?.end);
  if (!start || !end) return undefined;
  const coordinates = [start.line, start.character, end.line, end.character];
  if (!coordinates.every((coordinate) => Number.isInteger(coordinate) && Number(coordinate) >= 0)) {
    return undefined;
  }
  return {
    startLineNumber: Number(start.line) + 1,
    startColumn: Number(start.character) + 1,
    endLineNumber: Number(end.line) + 1,
    endColumn: Number(end.character) + 1,
  };
}

function documentSymbol(value: unknown): DocumentSymbolFields | undefined {
  const symbol = record(value);
  if (!symbol || typeof symbol.name !== 'string'
    || typeof symbol.kind !== 'number' || !Number.isInteger(symbol.kind)
    || symbol.kind < 1 || symbol.kind > 26) return undefined;
  const range = symbolRangeFields(symbol.range);
  const selectionRange = symbolRangeFields(symbol.selectionRange);
  if (!range || !selectionRange) return undefined;
  const children = Array.isArray(symbol.children)
    ? symbol.children.flatMap((child) => {
      const mapped = documentSymbol(child);
      return mapped ? [mapped] : [];
    })
    : [];
  return {
    name: symbol.name,
    detail: typeof symbol.detail === 'string' ? symbol.detail : '',
    kind: symbol.kind - 1,
    tags: Array.isArray(symbol.tags) ? symbol.tags.filter((tag): tag is number => tag === 1) : [],
    range,
    selectionRange,
    children,
  };
}

export function documentSymbolFields(value: unknown): DocumentSymbolFields[] {
  if (!Array.isArray(value)) return [];
  return value.flatMap((entry) => {
    const mapped = documentSymbol(entry);
    return mapped ? [mapped] : [];
  });
}
