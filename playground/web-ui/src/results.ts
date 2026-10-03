export interface RunResult {
  ok: boolean;
  values: unknown;
  stdout: string;
  errors: unknown;
}

export interface FormattedRunResult {
  succeeded: boolean;
  valuesText: string;
  valuesCount: string;
  stdoutText: string;
  stdoutCount: string;
  errorsText: string;
  errorsCount: string;
}

function jsonText(value: unknown): string {
  if (typeof value === 'string') return JSON.stringify(value, null, 2);
  try {
    return JSON.stringify(value, null, 2) ?? String(value);
  } catch {
    return String(value);
  }
}

function errorItems(errors: unknown): unknown[] {
  if (errors === undefined || errors === null || errors === '') return [];
  return Array.isArray(errors) ? errors : [errors];
}

export function formatRunResult(result: RunResult): FormattedRunResult {
  const hasValues = result.values !== undefined && result.values !== null;
  const values = Array.isArray(result.values)
    ? result.values
    : hasValues
      ? [result.values]
      : [];
  const stdoutText = typeof result.stdout === 'string' ? result.stdout : jsonText(result.stdout);
  const stdoutLines = stdoutText.length === 0 ? 0 : stdoutText.split('\n').length;
  const errors = errorItems(result.errors);

  return {
    succeeded: result.ok,
    valuesText: values.length === 0 ? '' : values.map(jsonText).join('\n\n'),
    valuesCount: values.length === 0 ? '—' : String(values.length),
    stdoutText,
    stdoutCount: stdoutLines === 0 ? '—' : `${stdoutLines} ${stdoutLines === 1 ? 'line' : 'lines'}`,
    errorsText: errors.map(jsonText).join('\n'),
    errorsCount: errors.length === 0 ? '—' : String(errors.length),
  };
}

export function formatThrownError(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}
