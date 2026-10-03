import type { RunResult } from './results';

export interface ReplEvaluation {
  kind: string;
  text: string;
}

export interface ReplSession {
  evaluate(line: string): ReplEvaluation | Promise<ReplEvaluation>;
}

export type WasmModule = {
  default: () => Promise<unknown>;
  run: (source: string) => RunResult | Promise<RunResult>;
  ReplSession?: new () => ReplSession;
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
    run: async (source) => wasm.run(source),
  };

  if (wasm.ReplSession) {
    runtime.createReplSession = () => new wasm.ReplSession!();
  }
  return runtime;
}

export function describeRuntimeLoadFailure(error: unknown): string {
  const detail = error instanceof Error ? error.message : String(error);
  if (detail.includes('orna_wasm.js') || detail.includes('Failed to fetch dynamically imported module')) {
    return 'WASM runtime not found. Build playground/orna-wasm with wasm-pack, then reload.';
  }
  return `WASM runtime failed to initialize: ${detail}`;
}
