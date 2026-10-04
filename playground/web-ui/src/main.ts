import * as monaco from 'monaco-editor/esm/vs/editor/editor.api.js';
import EditorWorker from 'monaco-editor/esm/vs/editor/editor.worker.js?worker';
import sampleSource from './sample.orna?raw';
import { registerOrnaLanguage } from './language';
import { formatRunResult, formatThrownError } from './results';
import { describeRuntimeLoadFailure, initializeRuntime, type PlaygroundRuntime } from './runtime';
import './styles.css';

// Vite serves Monaco's worker as a separate module so editor work stays off
// the main thread.
globalThis.MonacoEnvironment = {
  getWorker: () => new EditorWorker(),
};

monaco.editor.defineTheme('orna-dark', {
  base: 'vs-dark',
  inherit: true,
  rules: [
    { token: 'keyword.orna', foreground: '8be0a3', fontStyle: 'bold' },
    { token: 'comment.orna', foreground: '71837a', fontStyle: 'italic' },
    { token: 'string.orna', foreground: 'e9ba79' },
    { token: 'number.orna', foreground: 'c2a1ff' },
    { token: 'function.orna', foreground: '89c9ec' },
    { token: 'operator.orna', foreground: '9aaea4' },
    { token: 'delimiter.orna', foreground: 'a0b1a8' },
  ],
  colors: {
    'editor.background': '#101b17',
    'editor.foreground': '#d8e4dc',
    'editorLineNumber.foreground': '#56675e',
    'editorLineNumber.activeForeground': '#acbdb2',
    'editor.lineHighlightBackground': '#17251f',
    'editor.selectionBackground': '#30584280',
    'editorCursor.foreground': '#9be1ac',
    'editorIndentGuide.background1': '#26362e',
    'editorWidget.background': '#14221b',
  },
});

registerOrnaLanguage(monaco.languages);

function requiredElement<T extends Element>(selector: string): T {
  const element = document.querySelector<T>(selector);
  if (!element) {
    throw new Error(`The playground page is missing ${selector}.`);
  }
  return element;
}

const editorHost = requiredElement<HTMLElement>('#editor');
const runButton = requiredElement<HTMLButtonElement>('#run-button');
const status = requiredElement<HTMLElement>('#runtime-status');
const statusText = requiredElement<HTMLElement>('#runtime-status-text');
const executionState = requiredElement<HTMLElement>('#execution-state');
const cursorPosition = requiredElement<HTMLElement>('#cursor-position');
const valuesOutput = requiredElement<HTMLElement>('#values-output');
const valuesCount = requiredElement<HTMLElement>('#values-count');
const stdoutOutput = requiredElement<HTMLElement>('#stdout-output');
const stdoutCount = requiredElement<HTMLElement>('#stdout-count');
const errorsOutput = requiredElement<HTMLElement>('#errors-output');
const errorsCount = requiredElement<HTMLElement>('#errors-count');

const editor = monaco.editor.create(editorHost, {
  value: sampleSource.trimEnd(),
  language: 'orna',
  theme: 'orna-dark',
  automaticLayout: true,
  minimap: { enabled: false },
  scrollBeyondLastLine: false,
  fontFamily: '"JetBrains Mono", "SFMono-Regular", Consolas, monospace',
  fontSize: 14,
  lineHeight: 24,
  tabSize: 4,
  padding: { top: 22, bottom: 24 },
  renderLineHighlight: 'line',
  roundedSelection: false,
  overviewRulerBorder: false,
  lineNumbersMinChars: 3,
  guides: { indentation: true, bracketPairs: true },
  bracketPairColorization: { enabled: true },
});

editor.onDidChangeCursorPosition(({ position }) => {
  cursorPosition.textContent = `Ln ${position.lineNumber}, Col ${position.column}`;
});

function setStatus(label: string, state: 'loading' | 'ready' | 'error'): void {
  statusText.textContent = label;
  status.dataset.state = state;
}

function setOutput(target: HTMLElement, text: string, emptyLabel: string): void {
  if (text.length === 0) {
    const empty = document.createElement('span');
    empty.className = 'empty-result';
    empty.textContent = emptyLabel;
    target.replaceChildren(empty);
  } else {
    target.textContent = text;
  }
}

function showRuntimeError(message: string): void {
  setOutput(valuesOutput, '', 'Values returned by the program appear here.');
  setOutput(stdoutOutput, '', 'Program output appears here.');
  setOutput(errorsOutput, message, 'Diagnostics appear here.');
  valuesCount.textContent = '—';
  stdoutCount.textContent = '—';
  errorsCount.textContent = '1';
  errorsOutput.closest('.result-section')?.classList.add('has-errors');
}

function showResult(result: Awaited<ReturnType<PlaygroundRuntime['run']>>): void {
  const formatted = formatRunResult(result);
  setOutput(valuesOutput, formatted.valuesText, 'No values returned.');
  setOutput(stdoutOutput, formatted.stdoutText, 'No stdout.');
  setOutput(errorsOutput, formatted.errorsText, formatted.succeeded ? 'No errors.' : 'Execution failed without a diagnostic.');
  valuesCount.textContent = formatted.valuesCount;
  stdoutCount.textContent = formatted.stdoutCount;
  errorsCount.textContent = formatted.errorsCount;
  errorsOutput.closest('.result-section')?.classList.toggle('has-errors', !formatted.succeeded);
  executionState.textContent = formatted.succeeded ? 'Completed' : 'Failed';
  executionState.dataset.state = formatted.succeeded ? 'success' : 'error';
}

let runtime: PlaygroundRuntime | undefined;
let isRunning = false;

async function runSource(): Promise<void> {
  if (!runtime || isRunning) return;

  isRunning = true;
  runButton.disabled = true;
  runButton.classList.add('is-running');
  executionState.textContent = 'Running';
  executionState.dataset.state = 'running';
  setStatus('Executing source', 'loading');

  try {
    const result = await runtime.run(editor.getValue());
    showResult(result);
    setStatus(result.ok ? 'Runtime ready' : 'Execution returned errors', result.ok ? 'ready' : 'error');
  } catch (error) {
    const message = formatThrownError(error);
    showRuntimeError(message);
    executionState.textContent = 'Failed';
    executionState.dataset.state = 'error';
    setStatus('Execution failed', 'error');
  } finally {
    isRunning = false;
    runButton.disabled = false;
    runButton.classList.remove('is-running');
  }
}

runButton.addEventListener('click', () => void runSource());
editor.addCommand(monaco.KeyMod.CtrlCmd | monaco.KeyCode.Enter, () => void runSource());

setStatus('Loading runtime', 'loading');
void initializeRuntime()
  .then((loadedRuntime) => {
    runtime = loadedRuntime;
    runButton.disabled = false;
    executionState.textContent = 'Ready';
    executionState.dataset.state = 'ready';
    setStatus('Runtime ready', 'ready');
  })
  .catch((error: unknown) => {
    showRuntimeError(describeRuntimeLoadFailure(error));
    executionState.textContent = 'Unavailable';
    executionState.dataset.state = 'error';
    setStatus('Runtime unavailable', 'error');
  });

window.addEventListener('beforeunload', () => editor.dispose(), { once: true });
