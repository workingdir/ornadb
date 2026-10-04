import {
  LiveSession,
  formatCbor,
  renderPresent,
  runResultFromPresentation,
} from '/playground/assets/presentation.mjs';

const page = document.querySelector('#live-bridge');
const status = document.querySelector('#live-status');
const output = document.querySelector('#live-presentation');
const runEventsSource = JSON.parse(document.querySelector('#run-events-source').textContent);
let eventWatch;

function showPresentation(current) {
  const fragment = document.createDocumentFragment();
  renderPresent(current.present, document, fragment);
  output.replaceChildren(fragment);
  status.textContent = `Revision ${current.revision}`;
}

const session = new LiveSession(page.dataset.database, {
  onStatus(message) {
    status.textContent = message;
  },
  onPresentation: showPresentation,
});

const ready = (async () => {
  await session.connect();
  eventWatch = await session.watch(runEventsSource);
})();

globalThis.ornaPlaygroundRun = async source => {
  await ready;
  if (typeof source !== 'string') throw new Error('Run source must be text.');
  const response = await session.evaluate(source);
  const current = await session.refresh(eventWatch);
  const summary = runResultFromPresentation(current.present);
  if (summary) return summary;
  if (response.code === 18) {
    const statusCode = response.body.get(0n);
    const value = response.body.get(1n);
    return {
      ok: statusCode === 0n,
      values: statusCode === 0n && value !== null ? [formatCbor(value)] : [],
      stdout: '',
      errors: statusCode === 0n ? [] : [{ message: 'Evaluation failed', line: 1, col: 1 }],
    };
  }
  throw new Error('The Orna server returned no run result.');
};
window.dispatchEvent(new Event('orna:runtime-ready'));

void ready.catch(error => {
  status.textContent = error instanceof Error ? error.message : String(error);
});

window.addEventListener('pagehide', () => session.dispose(), { once: true });
