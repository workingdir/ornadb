export interface RunResult {
  ok: boolean;
  values: string[];
  stdout: string;
  errors: RunError[];
  ast?: string;
}

export interface RunError {
  message: string;
  line: number;
  col: number;
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

function formatError(error: RunError): string {
  return `${error.message} (line ${error.line}, col ${error.col})`;
}

export function formatRunResult(result: RunResult): FormattedRunResult {
  const stdoutLines = result.stdout.length === 0 ? 0 : result.stdout.split('\n').length;

  return {
    succeeded: result.ok,
    valuesText: result.values.join('\n\n'),
    valuesCount: result.values.length === 0 ? '—' : String(result.values.length),
    stdoutText: result.stdout,
    stdoutCount: stdoutLines === 0 ? '—' : `${stdoutLines} ${stdoutLines === 1 ? 'line' : 'lines'}`,
    errorsText: result.errors.map(formatError).join('\n'),
    errorsCount: result.errors.length === 0 ? '—' : String(result.errors.length),
  };
}

export function formatThrownError(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}
