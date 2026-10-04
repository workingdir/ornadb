import type { RunError, RunResult } from './results';

export interface ReplEvaluation {
  kind: string;
  text: string;
}

export interface ReplSession {
  evaluate(line: string): ReplEvaluation | Promise<ReplEvaluation>;
}

interface WasmReplSession {
  evaluate(line: string): string;
}

export type WasmModule = {
  default: () => Promise<unknown>;
  run: (source: string) => string;
  ReplSession?: new () => WasmReplSession;
};

export interface PlaygroundRuntime {
  run(source: string): Promise<RunResult>;
  createReplSession?: () => ReplSession;
}

type ModuleLoader = () => Promise<WasmModule>;

async function loadGeneratedModule(): Promise<WasmModule> {
  const moduleUrl = new URL(
    `${import.meta.env.BASE_URL}orna-wasm/pkg/orna_wasm.js`,
    globalThis.location.href,
  );
  return (await import(/* @vite-ignore */ moduleUrl.href)) as WasmModule;
}

export async function initializeRuntime(
  loadModule: ModuleLoader = loadGeneratedModule,
): Promise<PlaygroundRuntime> {
  const wasm = await loadModule();
  if (typeof wasm.default !== 'function' || typeof wasm.run !== 'function') {
    throw new Error('The WASM package does not expose init() and run(source).');
  }

  await wasm.default();
  const runtime: PlaygroundRuntime = {
    run: async (source) => decodeRunResult(wasm.run(source)),
  };
  if (wasm.ReplSession) {
    const WasmSession = wasm.ReplSession;
    runtime.createReplSession = () => {
      const session = new WasmSession();
      return {
        evaluate: async (line) => decodeReplEvaluation(session.evaluate(line)),
      };
    };
  }
  return runtime;
}

function decodeRunResult(serialized: string): RunResult {
  const result: unknown = JSON.parse(serialized);
  if (!isRunResult(result)) {
    throw new Error('The WASM runtime returned an invalid run result.');
  }
  return result;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

function isRunResult(value: unknown): value is RunResult {
  return isRecord(value) &&
    typeof value.ok === 'boolean' &&
    Array.isArray(value.values) &&
    value.values.every((entry) => typeof entry === 'string') &&
    typeof value.stdout === 'string' &&
    Array.isArray(value.errors) &&
    value.errors.every(isRunError) &&
    (value.ast === undefined || typeof value.ast === 'string');
}

function isRunError(value: unknown): value is RunError {
  return isRecord(value) &&
    typeof value.message === 'string' &&
    typeof value.line === 'number' && Number.isInteger(value.line) && value.line > 0 &&
    typeof value.col === 'number' && Number.isInteger(value.col) && value.col > 0;
}

function decodeReplEvaluation(serialized: string): ReplEvaluation {
  const evaluation: unknown = JSON.parse(serialized);
  if (
    typeof evaluation !== 'object' || evaluation === null ||
    typeof (evaluation as Record<string, unknown>).kind !== 'string' ||
    typeof (evaluation as Record<string, unknown>).text !== 'string'
  ) {
    throw new Error('The WASM runtime returned an invalid REPL result.');
  }
  return evaluation as ReplEvaluation;
}

export function describeRuntimeLoadFailure(error: unknown): string {
  const detail = error instanceof Error ? error.message : String(error);
  if (detail.includes('orna_wasm.js') || detail.includes('Failed to fetch dynamically imported module')) {
    return 'WASM runtime not found. Build playground/orna-wasm with wasm-pack, then reload.';
  }
  return `WASM runtime failed to initialize: ${detail}`;
}
