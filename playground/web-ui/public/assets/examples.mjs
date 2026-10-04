const catalog = document.querySelector('#example-catalog');
const filter = document.querySelector('#example-filter');
const list = document.querySelector('#example-list');
const status = document.querySelector('#catalog-status');

function isRecord(value) {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

function isExample(value) {
  return isRecord(value)
    && typeof value.name === 'string'
    && typeof value.path === 'string'
    && typeof value.source === 'string';
}

function sourceRecordUrl(revision, path) {
  const segments = path.split('/').map((segment) => encodeURIComponent(segment));
  return `/blob/${revision}/${segments.join('/')}`;
}

function renderExample(example, revision) {
  const item = document.createElement('li');
  item.className = 'catalog-item';
  item.dataset.search = `${example.name} ${example.path}`.toLocaleLowerCase();

  const heading = document.createElement('h2');
  heading.textContent = example.name;
  const path = document.createElement('p');
  path.className = 'catalog-path';
  const pathLabel = document.createElement('code');
  pathLabel.textContent = example.path;
  path.append(pathLabel);

  const source = document.createElement('pre');
  const code = document.createElement('code');
  code.textContent = example.source;
  source.append(code);

  const links = document.createElement('p');
  links.className = 'catalog-links';
  const edit = document.createElement('a');
  edit.href = `/playground/?example=${encodeURIComponent(example.path)}`;
  edit.textContent = 'Open in editor';
  const record = document.createElement('a');
  record.href = sourceRecordUrl(revision, example.path);
  record.textContent = 'View committed record';
  links.append(edit, document.createTextNode(' · '), record);

  item.append(heading, path, source, links);
  return item;
}

function applyFilter() {
  const query = filter.value.trim().toLocaleLowerCase();
  for (const item of list.children) {
    item.hidden = !item.dataset.search.includes(query);
  }
}

filter.addEventListener('input', applyFilter);

try {
  const response = await fetch('/api/examples');
  if (!response.ok) throw new Error(`Examples request failed (${response.status}).`);
  const payload = await response.json();
  if (!isRecord(payload) || !/^[0-9a-f]{40}$/.test(payload.revision ?? '')) {
    throw new Error('The examples response has no committed database revision.');
  }
  const examples = Array.isArray(payload.examples) ? payload.examples.filter(isExample) : [];
  if (examples.length === 0) {
    status.textContent = 'No committed examples are available.';
  } else {
    list.replaceChildren(...examples.map((example) => renderExample(example, payload.revision)));
    status.textContent = `${examples.length} committed ${examples.length === 1 ? 'example' : 'examples'} from revision ${payload.revision.slice(0, 12)}.`;
  }
} catch (error) {
  status.textContent = error instanceof Error ? error.message : 'The examples could not be loaded.';
} finally {
  catalog.setAttribute('aria-busy', 'false');
}
