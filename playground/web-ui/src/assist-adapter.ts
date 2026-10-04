export interface CompletionRankingFields {
  sortText?: string;
  preselect?: boolean;
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
