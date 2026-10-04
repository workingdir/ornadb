import { readdir, readFile, rm, writeFile, mkdir } from 'node:fs/promises';
import { extname, join, sep } from 'node:path';
import { fileURLToPath } from 'node:url';

const webUi = fileURLToPath(new URL('../', import.meta.url));
const repository = fileURLToPath(new URL('../../../', import.meta.url));
const distribution = join(webUi, 'dist');
const assetRows = join(repository, 'playground', 'Asset');
const maxAssetBytes = 8 * 1024 * 1024;
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
    const path = join(directory, child.name);
    const relativePath = prefix ? `${prefix}/${child.name}` : child.name;
    if (child.isDirectory()) files.push(...await collectFiles(path, relativePath));
    else if (child.isFile()) files.push(relativePath);
  }
  return files;
}

function rowString(value) {
  return JSON.stringify(value);
}

await mkdir(assetRows, { recursive: true });
const files = await collectFiles('');
if (!files.includes('index.html')) throw new Error('The Vite output has no index.html shell.');

const writtenRows = new Set();
let totalContentBytes = 0;
const extraAssets = [
  ['assets/presentation.mjs', join(repository, 'playground/shared/presentation.mjs')],
  ['assets/serve-home.mjs', join(repository, 'crates/orna-cli-v1/src/serve_home.mjs')],
  ['assets/serve-playground.mjs', join(repository, 'crates/orna-cli-v1/src/serve_playground.mjs')],
];
const sources = [
  ...files.map((path) => [path, join(distribution, path)]),
  ...extraAssets,
];
for (const [path, sourcePath] of sources) {
  const content = await readFile(sourcePath);
  if (content.byteLength > maxAssetBytes) {
    throw new Error(`Playground DB asset exceeds ${maxAssetBytes} bytes: ${path}`);
  }
  const mediaType = mediaTypes.get(extname(path).toLowerCase());
  if (!mediaType) throw new Error(`Unsupported playground DB asset type: ${path}`);
  let contentText;
  if (mediaType === 'application/wasm') {
    contentText = content.toString('base64');
  } else {
    contentText = content.toString('utf8');
    if (!Buffer.from(contentText, 'utf8').equals(content)) {
      throw new Error(`Playground DB assets must be UTF-8 text: ${path}`);
    }
  }

  const id = `asset-${Buffer.from(path).toString('hex')}`;
  const rowPath = join(assetRows, `${id}.orna`);
  const row = [
    `{ id: ${rowString(id)}, path: ${rowString(path.split(sep).join('/'))}, ` +
    `media_type: ${rowString(mediaType)}, content: ${rowString(contentText)} }\n`,
  ].join('');
  await writeFile(rowPath, row, 'utf8');
  writtenRows.add(rowPath);
  totalContentBytes += content.byteLength;
}

for (const name of await readdir(assetRows)) {
  const path = join(assetRows, name);
  if (name.startsWith('asset-') && name.endsWith('.orna') && !writtenRows.has(path)) {
    await rm(path);
  }
}

console.log(`Wrote ${writtenRows.size} committed playground Asset rows (${totalContentBytes} content bytes).`);
