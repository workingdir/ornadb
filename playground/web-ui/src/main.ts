import { exampleIndexForKey, isExample } from './example-feed';
import { formatRunResult, formatThrownError, type RunResult } from './results';
import { servedRuntime } from './runtime';
import './styles.css';

function requiredElement<T extends Element>(selector: string): T {
  const element = document.querySelector<T>(selector);
  if (!element) throw new Error(`The playground page is missing ${selector}.`);
  return element;
}

const examplesSelect = requiredElement<HTMLSelectElement>('#examples');
const exampleStatus = requiredElement<HTMLElement>('#example-status');
const sourceInput = requiredElement<HTMLTextAreaElement>('#source');
const runButton = requiredElement<HTMLButtonElement>('#run-button');
const editorStatus = requiredElement<HTMLElement>('#editor-status');
const executionState = requiredElement<HTMLElement>('#execution-state');
const stdoutOutput = requiredElement<HTMLElement>('#stdout-output');
const valuesOutput = requiredElement<HTMLElement>('#values-output');
const errorsOutput = requiredElement<HTMLElement>('#errors-output');
const outputStatus = requiredElement<HTMLElement>('#output-status');
const runtime = servedRuntime();
let activeRun = false;

function setOutput(target: HTMLElement, value: string, emptyLabel: string): void {
  target.textContent = value.length === 0 ? emptyLabel : value;
}

function showResult(result: RunResult): void {
  const formatted = formatRunResult(result);
  setOutput(stdoutOutput, formatted.stdoutText, 'No stdout was produced.');
  setOutput(valuesOutput, formatted.valuesText, 'No values were returned.');
  setOutput(errorsOutput, formatted.errorsText, result.ok ? 'No errors.' : 'Execution failed without a diagnostic.');
  executionState.textContent = result.ok ? 'Completed' : 'Failed';
  outputStatus.textContent = result.ok ? 'Execution completed.' : 'Execution returned errors.';
}

async function runSource(): Promise<void> {
  if (activeRun) return;
  activeRun = true;
  runButton.disabled = true;
  executionState.textContent = 'Running';
  outputStatus.textContent = 'Running source in the OrnaDB server runtime.';
  try {
    showResult(await runtime.run(sourceInput.value));
  } catch (error) {
    const message = formatThrownError(error);
    setOutput(errorsOutput, message, 'Execution failed.');
    executionState.textContent = 'Failed';
    outputStatus.textContent = 'The run failed.';
  } finally {
    activeRun = false;
    runButton.disabled = false;
  }
}

runButton.addEventListener('click', () => void runSource());
sourceInput.addEventListener('keydown', (event) => {
  if ((event.ctrlKey || event.metaKey) && event.key === 'Enter') {
    event.preventDefault();
    void runSource();
  }
});

function loadSelectedExample(): void {
  const option = examplesSelect.selectedOptions[0];
  const source = option?.dataset.source;
  if (source === undefined) return;
  sourceInput.value = source;
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
    const value = typeof payload === 'object' && payload !== null && !Array.isArray(payload)
      ? (payload as Record<string, unknown>).examples
      : undefined;
    const examples = Array.isArray(value) ? value.filter(isExample) : [];
    examplesSelect.replaceChildren();
    if (examples.length === 0) {
      const empty = document.createElement('option');
      empty.textContent = 'No database examples';
      examplesSelect.append(empty);
      examplesSelect.disabled = true;
      editorStatus.textContent = 'No committed examples are available. You can still write source.';
      exampleStatus.textContent = 'No committed examples are available.';
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
    examplesSelect.selectedIndex = 0;
    loadSelectedExample();
    editorStatus.textContent = `${examples.length} committed ${examples.length === 1 ? 'example' : 'examples'} loaded.`;
    runButton.disabled = false;
  } catch (error) {
    const option = document.createElement('option');
    option.textContent = 'Examples unavailable';
    examplesSelect.replaceChildren(option);
    examplesSelect.disabled = true;
    editorStatus.textContent = formatThrownError(error);
    exampleStatus.textContent = 'The committed examples could not be loaded.';
    runButton.disabled = false;
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

runButton.disabled = true;
void loadExamples();
