import { readdir, readFile, rm, writeFile, mkdir } from 'node:fs/promises';
import { extname, join, sep } from 'node:path';
import { fileURLToPath } from 'node:url';

const webUi = fileURLToPath(new URL('../', import.meta.url));
const repository = fileURLToPath(new URL('../../../', import.meta.url));
const distribution = join(webUi, 'dist');
const assetRows = join(repository, 'playground', 'Asset');
const entryRows = join(repository, 'playground', 'Entry');
const routeRows = join(repository, 'playground', 'Route');
const maxAssetBytes = 8 * 1024 * 1024;
const arguments_ = process.argv.slice(2);
if (arguments_.length > 1 || arguments_.some((argument) => argument !== '--check')) {
  throw new Error('Usage: node scripts/generate-db-assets.mjs [--check]');
}
const checkOnly = arguments_.includes('--check');
const mediaTypes = new Map([
  ['.html', 'text/html; charset=utf-8'],
  ['.js', 'text/javascript; charset=utf-8'],
  ['.mjs', 'text/javascript; charset=utf-8'],
  ['.css', 'text/css; charset=utf-8'],
  ['.json', 'application/json; charset=utf-8'],
  ['.svg', 'image/svg+xml'],
  ['.png', 'image/png'],
  ['.ico', 'image/x-icon'],
  ['.woff', 'font/woff'],
  ['.woff2', 'font/woff2'],
  ['.ttf', 'font/ttf'],
  ['.otf', 'font/otf'],
  ['.eot', 'application/vnd.ms-fontobject'],
  ['.wasm', 'application/wasm'],
]);

async function collectFiles(directory, prefix = '') {
  const children = await readdir(join(distribution, directory), { withFileTypes: true });
  const files = [];
  for (const child of children.sort((left, right) => left.name.localeCompare(right.name))) {
    if (child.name.startsWith('.')) continue;
    if (child.name.endsWith('.d.ts')) continue;
    const path = join(directory, child.name);
    const relativePath = prefix ? `${prefix}/${child.name}` : child.name;
    // This ignored local build output is not part of the database-served UI.
    if (child.isDirectory() && relativePath === 'lsp-wasm') continue;
    if (child.isDirectory()) files.push(...await collectFiles(path, relativePath));
    else if (child.isFile()) files.push(relativePath);
    else throw new Error('Unsupported playground build entry: ' + relativePath);
  }
  return files;
}

function ornaString(value) {
  let encoded = '"';
  for (const character of value) {
    const codePoint = character.codePointAt(0);
    if (character === '"') encoded += '\\"';
    else if (character === '\\') encoded += '\\\\';
    else if (character === '\n') encoded += '\\n';
    else if (character === '\r') encoded += '\\r';
    else if (character === '\t') encoded += '\\t';
    else if (character === '\0') encoded += '\\0';
    else if (character === '{' || character === '}' || codePoint < 0x20 || codePoint === 0x7f) {
      encoded += '\\u{' + codePoint.toString(16) + '}';
    } else encoded += character;
  }
  return encoded + '"';
}

function assetRow(id, path, mediaType, content) {
  return '{ id: ' + ornaString(id) + ', path: ' + ornaString(path) + ', '
    + 'media_type: ' + ornaString(mediaType) + ', content: ' + ornaString(content) + ' }\n';
}

function entryRow(id, assetPath, kind) {
  return `{ id: ${ornaString(id)}, asset_path: ${ornaString(assetPath)}, `
    + `kind: ${ornaString(kind)} }\n`;
}

function routeRow(id, path, entry) {
  return `{ id: ${ornaString(id)}, path: ${ornaString(path)}, `
    + `entry: ${ornaString(entry)} }\n`;
}

const extraAssets = [
  ['assets/presentation.mjs', join(repository, 'playground/shared/presentation.mjs')],
  ['assets/serve-home.mjs', join(repository, 'crates/orna-cli-v1/src/serve_home.mjs')],
  ['assets/serve-playground.mjs', join(repository, 'crates/orna-cli-v1/src/serve_playground.mjs')],
];

async function expectedRows() {
  const files = await collectFiles('');
  if (!files.includes('index.html')) throw new Error('The Vite output has no index.html shell.');

  const rows = new Map();
  const counts = { Asset: 0, Entry: 0, Route: 0 };
  let totalContentBytes = 0;
  function addRoute(path, entry) {
    const id = `route-${Buffer.from(path).toString('hex')}`;
    rows.set(join(routeRows, `${id}.orna`), routeRow(id, path, entry));
    counts.Route += 1;
  }

  rows.set(join(entryRows, 'entry-page.orna'), entryRow('entry-page', 'index.html', 'page'));
  rows.set(join(entryRows, 'entry-embed.orna'), entryRow('entry-embed', 'index.html', 'embed'));
  counts.Entry += 2;
  addRoute('/playground/', 'entry-page');
  addRoute('/playground/embed', 'entry-embed');
  const pages = [
    {
      routePath: '/playground/examples/',
      entryId: 'entry-example-catalog',
      assetPath: 'assets/examples.html',
      kind: 'asset',
    },
  ];
  for (const page of pages) {
    rows.set(
      join(entryRows, `${page.entryId}.orna`),
      entryRow(page.entryId, page.assetPath, page.kind),
    );
    counts.Entry += 1;
    addRoute(page.routePath, page.entryId);
  }

  const sources = [
    ...files.map((path) => [path, join(distribution, path)]),
    ...extraAssets,
  ];
  for (const [path, sourcePath] of sources) {
    const content = await readFile(sourcePath);
    if (content.byteLength > maxAssetBytes) {
      throw new Error('Playground DB asset exceeds ' + maxAssetBytes + ' bytes: ' + path);
    }
    const mediaType = mediaTypes.get(extname(path).toLowerCase());
    if (!mediaType) throw new Error('Unsupported playground DB asset type: ' + path);
    let contentText;
    if (mediaType === 'application/wasm') {
      contentText = content.toString('base64');
    } else {
      contentText = content.toString('utf8');
      if (!Buffer.from(contentText, 'utf8').equals(content)) {
        throw new Error('Playground DB assets must be UTF-8 text: ' + path);
      }
    }

    const normalizedPath = path.split(sep).join('/');
    const id = 'asset-' + Buffer.from(normalizedPath).toString('hex');
    const rowPath = join(assetRows, id + '.orna');
    rows.set(rowPath, assetRow(id, normalizedPath, mediaType, contentText));
    counts.Asset += 1;
    totalContentBytes += content.byteLength;

    if (normalizedPath !== 'index.html') {
      const entryId = `entry-asset-${Buffer.from(normalizedPath).toString('hex')}`;
      const entryPath = join(entryRows, `${entryId}.orna`);
      rows.set(entryPath, entryRow(entryId, normalizedPath, 'asset'));
      counts.Entry += 1;
      addRoute(`/playground/${normalizedPath}`, entryId);
    }
  }
  return { rows, totalContentBytes, counts };
}

try {
  const { rows, totalContentBytes, counts } = await expectedRows();
  const directories = [
    {
      path: assetRows,
      isGenerated: (name) => name.startsWith('asset-'),
    },
    {
      path: entryRows,
      isGenerated: (name) => name.startsWith('entry-asset-') || name === 'entry-example-catalog.orna',
    },
    {
      path: routeRows,
      isGenerated(name) {
        if (!name.startsWith('route-')) return false;
        const routeHex = name.slice('route-'.length, -'.orna'.length);
        if (!/^(?:[0-9a-f]{2})+$/.test(routeHex)) return false;
        const path = Buffer.from(routeHex, 'hex').toString('utf8');
        return path === '/playground/' || path === '/playground/embed'
          || path === '/playground/examples/'
          || path.startsWith('/playground/assets/');
      },
    },
  ];
  if (!checkOnly) {
    for (const directory of directories) await mkdir(directory.path, { recursive: true });
  }
  const stale = [];
  for (const directory of directories) {
    let entries = [];
    try {
      entries = await readdir(directory.path, { withFileTypes: true });
    } catch (error) {
      if (error.code !== 'ENOENT') throw error;
    }
    stale.push(...entries
      .filter((entry) => entry.isFile()
        && directory.isGenerated(entry.name)
        && entry.name.endsWith('.orna'))
      .map((entry) => join(directory.path, entry.name))
      .filter((rowPath) => !rows.has(rowPath)));
  }
  const changed = [];
  for (const [rowPath, expected] of rows) {
    let current;
    try {
      current = await readFile(rowPath, 'utf8');
    } catch (error) {
      if (error.code !== 'ENOENT') throw error;
    }
    if (current !== expected) changed.push([rowPath, expected]);
  }

  if (checkOnly) {
    if (stale.length || changed.length) {
      for (const rowPath of stale) console.error(`Stale playground DB row: ${rowPath}`);
      for (const [rowPath] of changed) console.error(`Missing or stale playground DB row: ${rowPath}`);
      process.exitCode = 1;
    } else {
      console.log(`Verified ${counts.Asset} Asset, ${counts.Entry} Entry, and ${counts.Route} Route rows (${totalContentBytes} asset bytes).`);
    }
  } else {
    for (const [rowPath, content] of changed) await writeFile(rowPath, content, 'utf8');
    for (const rowPath of stale) await rm(rowPath);
    console.log(`Wrote ${counts.Asset} Asset, ${counts.Entry} Entry, and ${counts.Route} Route rows (${totalContentBytes} asset bytes).`);
  }
} catch (error) {
  console.error(error.message);
  process.exitCode = 1;
}
