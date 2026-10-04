import { LiveSession, formatCbor, renderPresent } from '/assets/presentation.mjs';

const page = document.querySelector('[data-database]');
const form = document.querySelector('#playground-form');
const source = document.querySelector('#playground-source');
const run = document.querySelector('#run-source');
const status = document.querySelector('#playground-status');
const liveStatus = document.querySelector('#live-status');
const result = document.querySelector('#playground-result');
const presentation = document.querySelector('#live-presentation');
let session;
let running = false;

function showPresentation(current) {
  const fragment = document.createDocumentFragment();
  renderPresent(current.present, document, fragment);
  presentation.replaceChildren(fragment);
  liveStatus.textContent = `Revision ${current.revision}`;
}

function showResult(frame) {
  if (frame.code === 18) {
    const state = frame.body.get(0n);
    const value = frame.body.get(1n);
    result.textContent = state === 0n ? formatCbor(value) : `Run status ${formatCbor(state)}`;
    return;
  }
  result.textContent = 'The server returned a diagnostic.';
}

session = new LiveSession(page.dataset.database, {
  onStatus(message) {
    status.textContent = message;
    run.disabled = running || session?.presentation.current === null || session?.socket?.readyState !== 1;
  },
  onPresentation: showPresentation,
  onResult: showResult,
});

void session.connect().catch(error => {
  status.textContent = error instanceof Error ? error.message : String(error);
});

form.addEventListener('submit', async event => {
  event.preventDefault();
  if (!session || running) return;
  running = true;
  run.disabled = true;
  status.textContent = 'Running source…';
  try {
    const frame = await session.evaluate(source.value);
    showResult(frame);
    status.textContent = 'Run finished.';
  } catch (error) {
    status.textContent = error instanceof Error ? error.message : String(error);
  } finally {
    running = false;
    run.disabled = session.presentation.current === null || session.socket?.readyState !== 1;
  }
});

window.addEventListener('pagehide', () => session?.dispose(), { once: true });
