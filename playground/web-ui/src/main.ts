import * as monaco from 'monaco-editor/esm/vs/editor/editor.api.js';
import EditorWorker from 'monaco-editor/esm/vs/editor/editor.worker.js?worker';
import '../node_modules/monaco-editor/min/vs/editor/editor.main.css';
import { loadOrnaEditorConfig, registerOrnaLanguage } from './language';
import {
  completionRankingFields,
  hoverMarkdown,
  inlayHintFields,
  signatureHelpFields,
} from './assist-adapter';
import { exampleIndexForKey, exampleIndexForPath, isExample } from './example-feed';
import { formatRunResult, formatThrownError, type RunResult } from './results';
import { servedRuntime } from './runtime';
import './styles.css';

type LspAction = 'diagnostics' | 'completions' | 'hover' | 'signature_help' | 'inlay_hints';
type Position = { line: number; character: number };
type DocumentRange = { start: Position; end: Position };
type LspReply = { id: number; value?: unknown; error?: string };

globalThis.MonacoEnvironment = { getWorker: () => new EditorWorker() };
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

const editorHost = requiredElement<HTMLElement>('#source-editor');
const examplesSelect = requiredElement<HTMLSelectElement>('#examples');
const exampleStatus = requiredElement<HTMLElement>('#example-status');
const runButton = requiredElement<HTMLButtonElement>('#run-button');
const editorStatus = requiredElement<HTMLElement>('#editor-status');
const executionState = requiredElement<HTMLElement>('#execution-state');
const stdoutOutput = requiredElement<HTMLElement>('#stdout-output');
const valuesOutput = requiredElement<HTMLElement>('#values-output');
const errorsOutput = requiredElement<HTMLElement>('#errors-output');
const astOutput = requiredElement<HTMLElement>('#ast-output');
const outputStatus = requiredElement<HTMLElement>('#output-status');

let editor: monaco.editor.IStandaloneCodeEditor | undefined;
let editorReady = false;

const lspWorker = new Worker(new URL('./lsp-worker.ts', import.meta.url), { type: 'module' });
const pendingLsp = new Map<number, { resolve: (value: unknown) => void; reject: (error: Error) => void }>();
let nextLspId = 1;

let activeRun = false;
let examplesReady = false;
let liveRuntimeReady = typeof (globalThis as typeof globalThis & { ornaPlaygroundRun?: unknown }).ornaPlaygroundRun === 'function';
const runtime = servedRuntime();

function updateRunButton(): void {
  runButton.disabled = activeRun || !editorReady || !examplesReady || !liveRuntimeReady;
}

window.addEventListener('orna:runtime-ready', () => {
  liveRuntimeReady = true;
  updateRunButton();
}, { once: true });

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

function requestLsp(
  action: LspAction,
  source: string,
  position?: Position,
  range?: DocumentRange,
): Promise<unknown> {
  const id = nextLspId++;
  return new Promise((resolve, reject) => {
    pendingLsp.set(id, { resolve, reject });
    lspWorker.postMessage({ id, action, source, position, range });
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
        ...completionRankingFields(item),
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

monaco.languages.registerHoverProvider('orna', {
  async provideHover(model, position) {
    const response = asRecord(await requestLsp('hover', model.getValue(), {
      line: position.lineNumber - 1,
      character: position.column - 1,
    }));
    const text = hoverMarkdown(response?.contents);
    return text ? { contents: [{ value: text }] } : null;
  },
});

monaco.languages.registerSignatureHelpProvider('orna', {
  signatureHelpTriggerCharacters: ['(', ','],
  signatureHelpRetriggerCharacters: [','],
  async provideSignatureHelp(model, position) {
    const response = signatureHelpFields(await requestLsp('signature_help', model.getValue(), {
      line: position.lineNumber - 1,
      character: position.column - 1,
    }));
    if (!response) return null;
    return {
      value: response,
      dispose: () => undefined,
    };
  },
});

monaco.languages.registerInlayHintsProvider('orna', {
  displayName: 'Orna inlay hints',
  async provideInlayHints(model, range, token) {
    if (token.isCancellationRequested) return { hints: [], dispose: () => undefined };
    const response = await requestLsp('inlay_hints', model.getValue(), undefined, {
      start: {
        line: range.startLineNumber - 1,
        character: range.startColumn - 1,
      },
      end: {
        line: range.endLineNumber - 1,
        character: range.endColumn - 1,
      },
    });
    if (token.isCancellationRequested) return { hints: [], dispose: () => undefined };
    const hints = inlayHintFields(response).map((hint): monaco.languages.InlayHint => ({
      label: typeof hint.label === 'string'
        ? hint.label
        : hint.label.map((part) => ({ label: part.label, tooltip: part.tooltip })),
      position: {
        lineNumber: hint.position.line + 1,
        column: hint.position.character + 1,
      },
      ...(hint.kind === undefined
        ? {}
        : {
          kind: hint.kind === 1
            ? monaco.languages.InlayHintKind.Type
            : monaco.languages.InlayHintKind.Parameter,
        }),
      ...(hint.tooltip === undefined ? {} : { tooltip: { value: hint.tooltip } }),
      ...(hint.paddingLeft === undefined ? {} : { paddingLeft: hint.paddingLeft }),
      ...(hint.paddingRight === undefined ? {} : { paddingRight: hint.paddingRight }),
    }));
    return { hints, dispose: () => undefined };
  },
});

let diagnosticTimer: ReturnType<typeof setTimeout> | undefined;
async function updateDiagnostics(): Promise<void> {
  const activeEditor = editor;
  if (!activeEditor) return;
  try {
    const response = await requestLsp('diagnostics', activeEditor.getValue());
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
    const model = activeEditor.getModel();
    if (model) monaco.editor.setModelMarkers(model, 'orna-lsp', markers);
    editorStatus.textContent = markers.length === 0
      ? 'No syntax diagnostics'
      : `${markers.length} syntax ${markers.length === 1 ? 'diagnostic' : 'diagnostics'}`;
  } catch {
    editorStatus.textContent = 'Editor analysis unavailable';
  }
}

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
  const activeEditor = editor;
  if (activeRun || !editorReady || !examplesReady || !liveRuntimeReady || !activeEditor) return;
  activeRun = true;
  updateRunButton();
  executionState.textContent = 'Running';
  outputStatus.textContent = 'Running source in the OrnaDB server runtime.';
  try {
    const result = await executeSource(activeEditor.getValue());
    showResult(result);
  } catch (error) {
    const message = formatThrownError(error);
    setOutput(errorsOutput, message, 'Execution failed.');
    executionState.textContent = 'Failed';
    outputStatus.textContent = 'The run failed.';
  } finally {
    activeRun = false;
    updateRunButton();
  }
}

runButton.addEventListener('click', () => void runSource());

async function initializeEditor(): Promise<void> {
  const config = await loadOrnaEditorConfig();
  registerOrnaLanguage(monaco.languages, config);
  editor = monaco.editor.create(editorHost, {
    ...config.editorOptions,
    value: '',
    language: config.language.id,
    theme: 'orna-basic',
    inlayHints: { enabled: 'on' },
    ariaLabel: 'Orna source editor',
    automaticLayout: true,
    minimap: { enabled: false },
    scrollBeyondLastLine: false,
    fontSize: 14,
    lineNumbersMinChars: 3,
    renderLineHighlight: 'line',
  });
  editor.addCommand(monaco.KeyMod.CtrlCmd | monaco.KeyCode.Enter, () => void runSource());
  editor.onDidChangeModelContent(() => {
    if (diagnosticTimer) clearTimeout(diagnosticTimer);
    diagnosticTimer = setTimeout(() => void updateDiagnostics(), 250);
  });
  editorReady = true;
  editorStatus.textContent = 'Orna editor configuration loaded from the database.';
  updateRunButton();
  void updateDiagnostics();
}

function loadSelectedExample(): void {
  const option = examplesSelect.selectedOptions[0];
  const source = option?.dataset.source;
  const activeEditor = editor;
  if (source === undefined || !activeEditor) return;
  activeEditor.setValue(source);
  const name = option.textContent ?? 'Example';
  editorStatus.textContent = `${name} loaded. Edit the source or run it as-is.`;
  exampleStatus.textContent = `${name} loaded into the source editor.`;
}

examplesSelect.addEventListener('change', loadSelectedExample);
examplesSelect.addEventListener('keydown', (event) => {
  if (event.altKey || event.ctrlKey || event.metaKey) return;
  const nextIndex = exampleIndexForKey(event.key, examplesSelect.selectedIndex, examplesSelect.options.length);
  if (nextIndex === undefined) return;
  event.preventDefault();
  if (nextIndex === examplesSelect.selectedIndex) return;
  examplesSelect.selectedIndex = nextIndex;
  loadSelectedExample();
});

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
      editorStatus.textContent = 'No committed playground examples';
      exampleStatus.textContent = 'No committed examples are available.';
      examplesReady = true;
      updateRunButton();
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
    const requestedExample = new URLSearchParams(window.location.search).get('example');
    examplesSelect.selectedIndex = exampleIndexForPath(examples, requestedExample);
    loadSelectedExample();
    editorStatus.textContent = `${examples.length} committed ${examples.length === 1 ? 'example' : 'examples'} loaded.`;
    examplesReady = true;
    updateRunButton();
  } catch (error) {
    const option = document.createElement('option');
    option.textContent = 'Examples unavailable';
    examplesSelect.replaceChildren(option);
    editorStatus.textContent = formatThrownError(error);
    exampleStatus.textContent = 'The committed examples could not be loaded.';
    examplesReady = true;
    updateRunButton();
  }
}

const resultTabs = Array.from(document.querySelectorAll<HTMLButtonElement>('[data-output-tab]'));
const resultPanels = Array.from(document.querySelectorAll<HTMLElement>('[data-output-panel]'));

function activateResultTab(tab: HTMLButtonElement, moveFocus: boolean): void {
  const name = tab.dataset.outputTab;
  for (const candidate of resultTabs) {
    const selected = candidate === tab;
    candidate.setAttribute('aria-selected', String(selected));
    candidate.tabIndex = selected ? 0 : -1;
  }
  for (const panel of resultPanels) panel.hidden = panel.dataset.outputPanel !== name;
  if (moveFocus) tab.focus();
}

for (const tab of resultTabs) tab.addEventListener('click', () => activateResultTab(tab, false));
requiredElement<HTMLElement>('[role="tablist"]').addEventListener('keydown', (event) => {
  if (!(event instanceof KeyboardEvent) || resultTabs.length === 0) return;
  const current = resultTabs.indexOf(document.activeElement as HTMLButtonElement);
  if (current < 0) return;
  const index = event.key === 'ArrowRight'
    ? (current + 1) % resultTabs.length
    : event.key === 'ArrowLeft'
      ? (current + resultTabs.length - 1) % resultTabs.length
      : event.key === 'Home'
        ? 0
        : event.key === 'End'
          ? resultTabs.length - 1
          : -1;
  if (index < 0) return;
  event.preventDefault();
  activateResultTab(resultTabs[index], true);
});

updateRunButton();
void initializeEditor()
  .then(loadExamples)
  .catch((error: unknown) => {
    editorStatus.textContent = `The editor could not load: ${formatThrownError(error)}`;
    examplesReady = true;
    updateRunButton();
  });
window.addEventListener('beforeunload', () => {
  editor?.dispose();
  lspWorker.terminate();
}, { once: true });
