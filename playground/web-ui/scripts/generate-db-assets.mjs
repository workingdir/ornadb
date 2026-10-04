import { readdir, readFile, rm, writeFile, mkdir } from 'node:fs/promises';
import { extname, join, sep } from 'node:path';
import { fileURLToPath } from 'node:url';

const webUi = fileURLToPath(new URL('../', import.meta.url));
const repository = fileURLToPath(new URL('../../../', import.meta.url));
const distribution = join(webUi, 'dist');
const assetRows = join(repository, 'playground', 'Asset');
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

const extraAssets = [
  ['assets/presentation.mjs', join(repository, 'playground/shared/presentation.mjs')],
  ['assets/serve-home.mjs', join(repository, 'crates/orna-cli-v1/src/serve_home.mjs')],
  ['assets/serve-playground.mjs', join(repository, 'crates/orna-cli-v1/src/serve_playground.mjs')],
];

async function expectedRows() {
  const files = await collectFiles('');
  if (!files.includes('index.html')) throw new Error('The Vite output has no index.html shell.');

  const rows = new Map();
  let totalContentBytes = 0;
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
    totalContentBytes += content.byteLength;
  }
  return { rows, totalContentBytes };
}

try {
  const { rows, totalContentBytes } = await expectedRows();
  if (!checkOnly) await mkdir(assetRows, { recursive: true });
  let entries = [];
  try {
    entries = await readdir(assetRows, { withFileTypes: true });
  } catch (error) {
    if (error.code !== 'ENOENT') throw error;
  }
  const existing = entries
    .filter((entry) => entry.isFile() && entry.name.startsWith('asset-') && entry.name.endsWith('.orna'))
    .map((entry) => join(assetRows, entry.name));
  const stale = existing.filter((rowPath) => !rows.has(rowPath));
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
      for (const rowPath of stale) console.error('Stale playground Asset row: ' + rowPath);
      for (const [rowPath] of changed) console.error('Missing or stale playground Asset row: ' + rowPath);
      process.exitCode = 1;
    } else {
      console.log('Verified ' + rows.size + ' committed playground Asset rows (' + totalContentBytes + ' content bytes).');
    }
  } else {
    for (const [rowPath, content] of changed) await writeFile(rowPath, content, 'utf8');
    for (const rowPath of stale) await rm(rowPath);
    console.log('Wrote ' + rows.size + ' committed playground Asset rows (' + totalContentBytes + ' content bytes).');
  }
} catch (error) {
  console.error(error.message);
  process.exitCode = 1;
}
