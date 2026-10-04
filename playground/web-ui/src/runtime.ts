import type { RunResult } from './results';

export interface PlaygroundRuntime {
  run(source: string): Promise<RunResult>;
}

type ServedRun = (source: string) => Promise<unknown>;

export function createServedRuntime(run: ServedRun): PlaygroundRuntime {
  return {
    async run(source) {
      const result = await run(source);
      if (!isRunResult(result)) {
        throw new Error('The Orna server returned an invalid run result.');
      }
      return result;
    },
  };
}

function isRunResult(value: unknown): value is RunResult {
  if (typeof value !== 'object' || value === null || Array.isArray(value)) return false;
  const result = value as Record<string, unknown>;
  return typeof result.ok === 'boolean' &&
    Array.isArray(result.values) && result.values.every((entry) => typeof entry === 'string') &&
    typeof result.stdout === 'string' &&
    Array.isArray(result.errors) && result.errors.every((entry) => {
      if (typeof entry !== 'object' || entry === null || Array.isArray(entry)) return false;
      const error = entry as Record<string, unknown>;
      return typeof error.message === 'string' &&
        typeof error.line === 'number' && Number.isInteger(error.line) && error.line > 0 &&
        typeof error.col === 'number' && Number.isInteger(error.col) && error.col > 0;
    }) && (result.ast === undefined || typeof result.ast === 'string');
}

export function servedRuntime(): PlaygroundRuntime {
  return createServedRuntime(source => {
    const bridge = (globalThis as typeof globalThis & { ornaPlaygroundRun?: ServedRun }).ornaPlaygroundRun;
    if (typeof bridge !== 'function') {
      throw new Error('The Orna live runtime is not available. Open this page from orna serve.');
    }
    return bridge(source);
  });
}
