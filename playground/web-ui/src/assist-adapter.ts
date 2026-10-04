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
