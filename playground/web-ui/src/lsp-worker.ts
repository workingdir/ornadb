type Action = 'diagnostics' | 'completions' | 'hover' | 'signature_help';
type Request = {
  id: number;
  action: Action;
  source: string;
  position?: { line: number; character: number };
};
type Reply = { id: number; value?: unknown; error?: string };
type OrnaLspModule = {
  default: () => Promise<unknown>;
  diagnostics: (source: string) => string;
  completions: (source: string, line: number, character: number) => string;
  hover: (source: string, line: number, character: number) => string;
  signature_help: (source: string, line: number, character: number) => string;
};

const workerScope = globalThis as unknown as {
  addEventListener: (type: 'message', listener: (event: MessageEvent<Request>) => void) => void;
  postMessage: (reply: Reply) => void;
};

let modulePromise: Promise<OrnaLspModule> | undefined;
function loadLsp(): Promise<OrnaLspModule> {
  modulePromise ??= (async () => {
    const packageUrl = new URL(`${import.meta.env.BASE_URL}lsp-wasm/orna_lsp.js`, self.location.href);
    const module = await import(/* @vite-ignore */ packageUrl.href) as OrnaLspModule;
    await module.default();
    return module;
  })();
  return modulePromise;
}

workerScope.addEventListener('message', async ({ data }) => {
  try {
    const lsp = await loadLsp();
    const line = data.position?.line ?? 0;
    const character = data.position?.character ?? 0;
    const serialized = data.action === 'diagnostics'
      ? lsp.diagnostics(data.source)
      : data.action === 'completions'
        ? lsp.completions(data.source, line, character)
        : data.action === 'hover'
          ? lsp.hover(data.source, line, character)
          : lsp.signature_help(data.source, line, character);
    workerScope.postMessage({ id: data.id, value: JSON.parse(serialized) });
  } catch (error) {
    workerScope.postMessage({
      id: data.id,
      error: error instanceof Error ? error.message : String(error),
    });
  }
});
