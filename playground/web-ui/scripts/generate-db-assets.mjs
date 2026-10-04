import { readdir, readFile, rm, writeFile, mkdir } from 'node:fs/promises';
import { extname, join, sep } from 'node:path';
import { fileURLToPath } from 'node:url';

const webUi = fileURLToPath(new URL('../', import.meta.url));
const repository = fileURLToPath(new URL('../../../', import.meta.url));
const distribution = join(webUi, 'dist');
const assetRows = join(repository, 'playground', 'Asset');
const themeRows = join(repository, 'playground', 'Theme');
const layoutRows = join(repository, 'playground', 'Layout');
const maxAssetBytes = 2 * 1024 * 1024;
const maxStyleBytes = 256 * 1024;
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
  return JSON.stringify(value).replaceAll('{', '\\u{7b}');
}

async function writeRecord(directory, filename, record) {
  await mkdir(directory, { recursive: true });
  const fields = Object.entries(record)
    .map(([name, value]) => `${name}: ${rowString(value)}`)
    .join(', ');
  await writeFile(join(directory, filename), `{ ${fields} }\n`, 'utf8');
}

await Promise.all([
  mkdir(assetRows, { recursive: true }),
  mkdir(themeRows, { recursive: true }),
  mkdir(layoutRows, { recursive: true }),
]);
const files = await collectFiles('');
if (!files.includes('index.html')) throw new Error('The Vite output has no index.html shell.');

const writtenRows = new Set();
let totalContentBytes = 0;
for (const path of files) {
  if (extname(path).toLowerCase() === '.css') continue;
  const content = await readFile(join(distribution, path));
  if (content.byteLength > maxAssetBytes) {
    throw new Error(`Playground DB asset exceeds ${maxAssetBytes} bytes: ${path}`);
  }
  const mediaType = mediaTypes.get(extname(path).toLowerCase());
  if (!mediaType) throw new Error(`Unsupported playground DB asset type: ${path}`);
  let contentText = content.toString('utf8');
  if (!Buffer.from(contentText, 'utf8').equals(content)) {
    throw new Error(`Playground DB assets must be UTF-8 text: ${path}`);
  }
  if (path === 'index.html') {
    const stylesheetLinks = contentText.match(/<link\b[^>]*\brel="stylesheet"[^>]*>/g) ?? [];
    if (stylesheetLinks.length !== 1) {
      throw new Error(`Expected one bundled stylesheet link in the generated shell, found ${stylesheetLinks.length}.`);
    }
    contentText = contentText.replace(
      stylesheetLinks[0],
      '<link id="playground-theme" rel="stylesheet" href="/playground/theme.css">' +
      '<link id="playground-layout" rel="stylesheet" href="/playground/layout.css">',
    );
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

for (const [directory, filename, id, name, sourcePath] of [
  [themeRows, 'wiki-basic.orna', 'wiki-basic', 'Wiki basic', 'theme.css'],
  [layoutRows, 'responsive.orna', 'responsive', 'Responsive layout', 'layout.css'],
]) {
  const css = await readFile(join(webUi, 'src', sourcePath));
  if (css.byteLength > maxStyleBytes) {
    throw new Error(`Playground ${sourcePath} exceeds ${maxStyleBytes} bytes.`);
  }
  const cssText = css.toString('utf8');
  if (!Buffer.from(cssText, 'utf8').equals(css)) {
    throw new Error(`Playground ${sourcePath} must be UTF-8 text.`);
  }
  await writeRecord(directory, filename, { id, name, css: cssText });
}

console.log(
  `Wrote ${writtenRows.size} committed playground Asset rows (${totalContentBytes} content bytes), ` +
  'one Theme row, and one Layout row.',
);
