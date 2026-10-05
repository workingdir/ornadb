type Action = 'diagnostics' | 'completions' | 'hover' | 'definition' | 'references' | 'signature_help' | 'inlay_hints';
type Position = { line: number; character: number };
type Request = {
  id: number;
  action: Action;
  source: string;
  position?: Position;
  range?: { start: Position; end: Position };
  includeDeclaration?: boolean;
};
type Reply = { id: number; value?: unknown; error?: string };
type OrnaLspModule = {
  default: () => Promise<unknown>;
  diagnostics: (source: string) => string;
  completions: (source: string, line: number, character: number) => string;
  hover: (source: string, line: number, character: number) => string;
  definition: (source: string, line: number, character: number) => string;
  references: (source: string, line: number, character: number, includeDeclaration: boolean) => string;
  signature_help: (source: string, line: number, character: number) => string;
  inlay_hints: (
    source: string,
    startLine: number,
    startCharacter: number,
    endLine: number,
    endCharacter: number,
  ) => string;
};

const workerScope = globalThis as unknown as {
  addEventListener: (type: 'message', listener: (event: MessageEvent<Request>) => void) => void;
  postMessage: (reply: Reply) => void;
};

let modulePromise: Promise<OrnaLspModule> | undefined;
function loadLsp(): Promise<OrnaLspModule> {
  modulePromise ??= (async () => {
    const packageUrl = new URL(`${import.meta.env.BASE_URL}assets/lsp-wasm/orna_lsp.js`, self.location.href);
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
    let serialized: string;
    switch (data.action) {
      case 'diagnostics':
        serialized = lsp.diagnostics(data.source);
        break;
      case 'completions':
        serialized = lsp.completions(data.source, line, character);
        break;
      case 'hover':
        serialized = lsp.hover(data.source, line, character);
        break;
      case 'definition':
        serialized = lsp.definition(data.source, line, character);
        break;
      case 'references':
        serialized = lsp.references(data.source, line, character, data.includeDeclaration ?? false);
        break;
      case 'signature_help':
        serialized = lsp.signature_help(data.source, line, character);
        break;
      case 'inlay_hints': {
        const lines = data.source.split(/\r?\n/);
        const start = data.range?.start ?? { line: 0, character: 0 };
        const end = data.range?.end ?? {
          line: lines.length - 1,
          character: [...(lines.at(-1) ?? '')].length,
        };
        serialized = lsp.inlay_hints(
            data.source,
          start.line,
          start.character,
          end.line,
          end.character,
        );
        break;
      }
    }
    workerScope.postMessage({ id: data.id, value: JSON.parse(serialized) });
  } catch (error) {
    workerScope.postMessage({
      id: data.id,
      error: error instanceof Error ? error.message : String(error),
    });
  }
});
