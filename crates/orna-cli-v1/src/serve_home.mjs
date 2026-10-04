import { LiveSession, renderPresent } from '/assets/presentation.mjs';

const page = document.querySelector('[data-database]');
const status = document.querySelector('#live-status');
const output = document.querySelector('#live-presentation');
let session;

function showPresentation(current) {
  const fragment = document.createDocumentFragment();
  renderPresent(current.present, document, fragment);
  output.replaceChildren(fragment);
}

session = new LiveSession(page.dataset.database, {
  onStatus(message) {
    status.textContent = message;
  },
  onPresentation(current) {
    showPresentation(current);
  },
});

void session.connect().catch(error => {
  status.textContent = error instanceof Error ? error.message : String(error);
});

window.addEventListener('pagehide', () => session?.dispose(), { once: true });
