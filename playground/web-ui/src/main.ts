import * as monaco from 'monaco-editor/esm/vs/editor/editor.api.js';
import EditorWorker from 'monaco-editor/esm/vs/editor/editor.worker.js?worker';
import { registerOrnaLanguage } from './language';
import { formatRunResult, formatThrownError, type RunResult } from './results';
import { servedRuntime } from './runtime';
import './styles.css';

type LspAction = 'diagnostics' | 'completions' | 'hover' | 'signature_help';
type Position = { line: number; character: number };
type LspReply = { id: number; value?: unknown; error?: string };
type Example = { name: string; path: string; source: string };

globalThis.MonacoEnvironment = { getWorker: () => new EditorWorker() };
registerOrnaLanguage(monaco.languages);
const pageStyle = getComputedStyle(document.documentElement);
monaco.editor.defineTheme('orna-basic', {
  base: 'vs',
  inherit: true,
  rules: [],
  colors: {
    'editor.background': pageStyle.getPropertyValue('--background').trim() || '#fff',
    'editor.foreground': pageStyle.getPropertyValue('--text').trim() || '#202122',
  },
});

function requiredElement<T extends Element>(selector: string): T {
  const element = document.querySelector<T>(selector);
  if (!element) throw new Error(`The playground page is missing ${selector}.`);
  return element;
}

const editorHost = requiredElement<HTMLElement>('#editor');
const examplesSelect = requiredElement<HTMLSelectElement>('#examples');
const runButton = requiredElement<HTMLButtonElement>('#run-button');
const stopButton = requiredElement<HTMLButtonElement>('#stop-button');
const editorStatus = requiredElement<HTMLElement>('#editor-status');
const executionState = requiredElement<HTMLElement>('#execution-state');
const stdoutOutput = requiredElement<HTMLElement>('#stdout-output');
const valuesOutput = requiredElement<HTMLElement>('#values-output');
const errorsOutput = requiredElement<HTMLElement>('#errors-output');
const astOutput = requiredElement<HTMLElement>('#ast-output');
const outputStatus = requiredElement<HTMLElement>('#output-status');

const editor = monaco.editor.create(editorHost, {
  value: '',
  language: 'orna',
  theme: 'orna-basic',
  automaticLayout: true,
  minimap: { enabled: false },
  scrollBeyondLastLine: false,
  fontSize: 14,
  tabSize: 4,
  lineNumbersMinChars: 3,
  renderLineHighlight: 'line',
});

const lspWorker = new Worker(new URL('./lsp-worker.ts', import.meta.url), { type: 'module' });
const pendingLsp = new Map<number, { resolve: (value: unknown) => void; reject: (error: Error) => void }>();
let nextLspId = 1;

let activeRun = false;
const runtime = servedRuntime();

async function executeSource(source: string): Promise<RunResult> {
  return runtime.run(source);
}

lspWorker.addEventListener('message', ({ data }: MessageEvent<LspReply>) => {
  const pending = pendingLsp.get(data.id);
  if (!pending) return;
  pendingLsp.delete(data.id);
  if (data.error) pending.reject(new Error(data.error));
  else pending.resolve(data.value);
});

lspWorker.addEventListener('error', (event) => {
  for (const pending of pendingLsp.values()) pending.reject(new Error(event.message));
  pendingLsp.clear();
  editorStatus.textContent = 'Editor analysis unavailable';
});

function requestLsp(action: LspAction, source: string, position?: Position): Promise<unknown> {
  const id = nextLspId++;
  return new Promise((resolve, reject) => {
    pendingLsp.set(id, { resolve, reject });
    lspWorker.postMessage({ id, action, source, position });
  });
}

function asRecord(value: unknown): Record<string, unknown> | undefined {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
    ? value as Record<string, unknown>
    : undefined;
}

function asArray(value: unknown): unknown[] {
  return Array.isArray(value) ? value : [];
}

function completionKind(kind: unknown): monaco.languages.CompletionItemKind | undefined {
  if (typeof kind !== 'number' || !Number.isInteger(kind) || kind < 1 || kind > 25) return undefined;
  return (kind - 1) as monaco.languages.CompletionItemKind;
}

monaco.languages.registerCompletionItemProvider('orna', {
  triggerCharacters: ['.', '('],
  async provideCompletionItems(model, position) {
    const response = await requestLsp('completions', model.getValue(), {
      line: position.lineNumber - 1,
      character: position.column - 1,
    });
    const word = model.getWordUntilPosition(position);
    const range = new monaco.Range(position.lineNumber, word.startColumn, position.lineNumber, word.endColumn);
    const suggestions = asArray(response).flatMap((entry) => {
      const item = asRecord(entry);
      if (!item || typeof item.label !== 'string') return [];
      const suggestion: monaco.languages.CompletionItem = {
        label: item.label,
        kind: completionKind(item.kind) ?? monaco.languages.CompletionItemKind.Text,
        insertText: typeof item.insertText === 'string' ? item.insertText : item.label,
        range,
      };
      if (item.detail) suggestion.detail = String(item.detail);
      if (item.documentation) {
        suggestion.documentation = typeof item.documentation === 'string'
          ? item.documentation
          : String(asRecord(item.documentation)?.value ?? '');
      }
      if (item.insertTextFormat === 2) {
        suggestion.insertTextRules = monaco.languages.CompletionItemInsertTextRule.InsertAsSnippet;
      }
      return [suggestion];
    });
    return { suggestions };
  },
});

function markdownContents(value: unknown): string {
  if (typeof value === 'string') return value;
  if (Array.isArray(value)) return value.map(markdownContents).filter(Boolean).join('\n\n');
  const record = asRecord(value);
  if (!record) return '';
  if (typeof record.value === 'string') {
    if (typeof record.language === 'string') return `\`\`\`${record.language}\n${record.value}\n\`\`\``;
    return record.value;
  }
  return '';
}

monaco.languages.registerHoverProvider('orna', {
  async provideHover(model, position) {
    const response = asRecord(await requestLsp('hover', model.getValue(), {
      line: position.lineNumber - 1,
      character: position.column - 1,
    }));
    const text = markdownContents(response?.contents);
    return text ? { contents: [{ value: text }] } : null;
  },
});

monaco.languages.registerSignatureHelpProvider('orna', {
  signatureHelpTriggerCharacters: ['(', ','],
  signatureHelpRetriggerCharacters: [','],
  async provideSignatureHelp(model, position) {
    const response = asRecord(await requestLsp('signature_help', model.getValue(), {
      line: position.lineNumber - 1,
      character: position.column - 1,
    }));
    if (!response) return null;
    const signatures = asArray(response.signatures).flatMap((entry) => {
      const signature = asRecord(entry);
      if (!signature || typeof signature.label !== 'string') return [];
      const parameters = asArray(signature.parameters).flatMap((parameterEntry) => {
        const parameter = asRecord(parameterEntry);
        if (!parameter) return [];
        const label = parameter.label;
        if (typeof label !== 'string' && !Array.isArray(label)) return [];
        const mapped: monaco.languages.ParameterInformation = { label: label as string | [number, number] };
        if (parameter.documentation) mapped.documentation = markdownContents(parameter.documentation);
        return [mapped];
      });
      const mapped: monaco.languages.SignatureInformation = {
        label: signature.label,
        parameters,
      };
      if (signature.documentation) mapped.documentation = markdownContents(signature.documentation);
      return [mapped];
    });
    if (signatures.length === 0) return null;
    const activeSignature = typeof response.activeSignature === 'number' ? response.activeSignature : 0;
    const activeParameter = typeof response.activeParameter === 'number' ? response.activeParameter : 0;
    return {
      value: { signatures, activeSignature, activeParameter },
      dispose: () => undefined,
    };
  },
});

let diagnosticTimer: ReturnType<typeof setTimeout> | undefined;
async function updateDiagnostics(): Promise<void> {
  try {
    const response = await requestLsp('diagnostics', editor.getValue());
    const markers = asArray(response).flatMap((entry) => {
      const item = asRecord(entry);
      const range = asRecord(item?.range);
      const start = asRecord(range?.start);
      const end = asRecord(range?.end);
      if (!item || !start || !end || typeof item.message !== 'string') return [];
      const severity = item.severity === 2
        ? monaco.MarkerSeverity.Warning
        : item.severity === 3
          ? monaco.MarkerSeverity.Info
          : item.severity === 4
            ? monaco.MarkerSeverity.Hint
            : monaco.MarkerSeverity.Error;
      return [{
        startLineNumber: Number(start.line) + 1,
        startColumn: Number(start.character) + 1,
        endLineNumber: Number(end.line) + 1,
        endColumn: Number(end.character) + 1,
        message: item.message,
        severity,
        source: typeof item.source === 'string' ? item.source : 'orna-lsp',
        code: typeof item.code === 'string' || typeof item.code === 'number' ? String(item.code) : undefined,
      }];
    });
    const model = editor.getModel();
    if (model) monaco.editor.setModelMarkers(model, 'orna-lsp', markers);
    editorStatus.textContent = markers.length === 0
      ? 'No syntax diagnostics'
      : `${markers.length} syntax ${markers.length === 1 ? 'diagnostic' : 'diagnostics'}`;
  } catch {
    editorStatus.textContent = 'Editor analysis unavailable';
  }
}

editor.onDidChangeModelContent(() => {
  if (diagnosticTimer) clearTimeout(diagnosticTimer);
  diagnosticTimer = setTimeout(() => void updateDiagnostics(), 250);
});
void updateDiagnostics();

function setOutput(target: HTMLElement, value: string, emptyLabel: string): void {
  target.textContent = value.length === 0 ? emptyLabel : value;
}

function showResult(result: RunResult): void {
  const formatted = formatRunResult(result);
  setOutput(stdoutOutput, formatted.stdoutText, 'No stdout was produced.');
  setOutput(valuesOutput, formatted.valuesText, 'No values were returned.');
  setOutput(errorsOutput, formatted.errorsText, result.ok ? 'No errors.' : 'Execution failed without a diagnostic.');
  setOutput(astOutput, result.ast ?? '', 'No syntax tree was returned.');
  executionState.textContent = result.ok ? 'Completed' : 'Failed';
  outputStatus.textContent = result.ok ? 'Execution completed.' : 'Execution returned errors.';
}

async function runSource(): Promise<void> {
  if (activeRun) return;
  activeRun = true;
  runButton.disabled = true;
  stopButton.disabled = false;
  executionState.textContent = 'Running';
  outputStatus.textContent = 'Running source in the OrnaDB server runtime.';
  try {
    const result = await executeSource(editor.getValue());
    showResult(result);
  } catch (error) {
    const message = formatThrownError(error);
    setOutput(errorsOutput, message, 'Execution failed.');
    executionState.textContent = 'Failed';
    outputStatus.textContent = 'The run failed.';
  } finally {
    activeRun = false;
    runButton.disabled = false;
    stopButton.disabled = true;
  }
}

runButton.addEventListener('click', () => void runSource());
stopButton.addEventListener('click', () => {
  if (!activeRun) return;
  executionState.textContent = 'Stop requested';
  outputStatus.textContent = 'The live API does not yet cancel server-side execution.';
});
editor.addCommand(monaco.KeyMod.CtrlCmd | monaco.KeyCode.Enter, () => void runSource());

function isExample(value: unknown): value is Example {
  const example = asRecord(value);
  return Boolean(example) && typeof example?.name === 'string' &&
    typeof example?.path === 'string' && typeof example?.source === 'string';
}

async function loadExamples(): Promise<void> {
  try {
    const response = await fetch('/api/examples');
    if (!response.ok) throw new Error(`Examples request failed (${response.status}).`);
    const payload: unknown = await response.json();
    const examples = asArray(asRecord(payload)?.examples).filter(isExample);
    examplesSelect.replaceChildren();
    if (examples.length === 0) {
      const empty = document.createElement('option');
      empty.textContent = 'No database examples';
      examplesSelect.append(empty);
      examplesSelect.disabled = true;
      editorStatus.textContent = 'No database examples available';
      runButton.disabled = false;
      return;
    }
    for (const example of examples) {
      const option = document.createElement('option');
      option.value = example.path;
      option.textContent = example.name;
      option.dataset.source = example.source;
      examplesSelect.append(option);
    }
    examplesSelect.disabled = false;
    editor.setValue(examples[0].source);
    runButton.disabled = false;
  } catch (error) {
    const option = document.createElement('option');
    option.textContent = 'Examples unavailable';
    examplesSelect.replaceChildren(option);
    editorStatus.textContent = formatThrownError(error);
    runButton.disabled = false;
  }
}

examplesSelect.addEventListener('change', () => {
  const source = examplesSelect.selectedOptions[0]?.dataset.source;
  if (source !== undefined) editor.setValue(source);
});

for (const tab of document.querySelectorAll<HTMLButtonElement>('[data-output-tab]')) {
  tab.addEventListener('click', () => {
    const name = tab.dataset.outputTab;
    for (const candidate of document.querySelectorAll<HTMLButtonElement>('[data-output-tab]')) {
      candidate.setAttribute('aria-selected', String(candidate === tab));
      candidate.tabIndex = candidate === tab ? 0 : -1;
    }
    for (const panel of document.querySelectorAll<HTMLElement>('[data-output-panel]')) {
      panel.hidden = panel.dataset.outputPanel !== name;
    }
  });
}

runButton.disabled = false;
void loadExamples();
window.addEventListener('beforeunload', () => {
  editor.dispose();
  lspWorker.terminate();
}, { once: true });
