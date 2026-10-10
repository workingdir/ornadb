import { LiveSession, renderPresent, runResultFromPresentation } from '/devtools/presentation.mjs';

const page = document.querySelector('#live-repl');
const source = document.querySelector('#repl-source');
const runButton = document.querySelector('#repl-run');
const runStatus = document.querySelector('#repl-status');
const runOutput = document.querySelector('#repl-events');
const liveStatus = document.querySelector('#live-status');
const presentationOutput = document.querySelector('#live-presentation');
const runEventsSource = JSON.parse(document.querySelector('#run-events-source').textContent);
let eventWatch;
let running = false;

function showPresentation(current) {
  const fragment = document.createDocumentFragment();
  renderPresent(current.present, document, fragment);
  presentationOutput.replaceChildren(fragment);
  liveStatus.textContent = `Revision ${current.revision}`;
}

function showRun(current) {
  const result = runResultFromPresentation(current.present);
  if (!result) return;
  const lines = [];
  if (result.stdout) lines.push(result.stdout);
  lines.push(...result.values);
  lines.push(...result.errors.map(error => `${error.message} (${error.line}:${error.col})`));
  runOutput.textContent = lines.join('\n') || (result.ok ? 'Run completed without output.' : 'Run failed.');
  runStatus.textContent = result.ok ? 'Run completed.' : 'Run failed.';
}

const session = new LiveSession(page.dataset.database, {
  onStatus(message) {
    liveStatus.textContent = message;
    runButton.disabled = running || !eventWatch || session.socket?.readyState !== 1;
  },
  onPresentation: showPresentation,
});

async function start() {
  await session.connect();
  eventWatch = await session.watch(runEventsSource, {
    onPresentation: showRun,
  });
  runButton.disabled = false;
  runStatus.textContent = 'Ready.';
}

runButton.addEventListener('click', async () => {
  if (running || !eventWatch) return;
  running = true;
  runButton.disabled = true;
  runStatus.textContent = 'Running source…';
  try {
    await session.evaluate(source.value);
    showRun(await session.refresh(eventWatch));
  } catch (error) {
    runStatus.textContent = error instanceof Error ? error.message : String(error);
  } finally {
    running = false;
    runButton.disabled = !eventWatch || session.socket?.readyState !== 1;
  }
});

void start().catch(error => {
  runStatus.textContent = error instanceof Error ? error.message : String(error);
});

window.addEventListener('pagehide', () => session.dispose(), { once: true });
