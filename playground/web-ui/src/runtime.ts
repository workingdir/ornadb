import type { RunResult } from './results';

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
    window.location.href,
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
  if (
    typeof result !== 'object' || result === null ||
    typeof (result as Record<string, unknown>).ok !== 'boolean' ||
    !Array.isArray((result as Record<string, unknown>).values) ||
    typeof (result as Record<string, unknown>).stdout !== 'string' ||
    !Array.isArray((result as Record<string, unknown>).errors)
  ) {
    throw new Error('The WASM runtime returned an invalid run result.');
  }
  return result as RunResult;
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
