import { readdir, readFile, mkdir, rm, writeFile } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const repositoryRoot = fileURLToPath(new URL('../../../', import.meta.url));
const distributionRoot = path.join(repositoryRoot, 'playground/web-ui/dist');
const recordsRoot = path.join(repositoryRoot, 'playground/Asset');
const maxAssetBytes = 8 * 1024 * 1024;
const maxAssetRows = 1000;
const checkOnly = process.argv.includes('--check');

const mimeTypes = new Map([
  ['.html', 'text/html; charset=utf-8'],
  ['.js', 'text/javascript; charset=utf-8'],
  ['.mjs', 'text/javascript; charset=utf-8'],
  ['.css', 'text/css; charset=utf-8'],
  ['.json', 'application/json; charset=utf-8'],
  ['.wasm', 'application/wasm'],
  ['.svg', 'image/svg+xml'],
  ['.png', 'image/png'],
  ['.ico', 'image/x-icon'],
  ['.woff', 'font/woff'],
  ['.woff2', 'font/woff2'],
  ['.ttf', 'font/ttf'],
  ['.otf', 'font/otf'],
  ['.eot', 'application/vnd.ms-fontobject'],
]);

function isSafeKey(key) {
  return key.length > 0
    && !key.startsWith('/')
    && !key.endsWith('/')
    && !key.includes('\\')
    && !key.includes('\0')
    && key.split('/').every((part) => part !== '' && part !== '.' && part !== '..')
    && [...key].every((character) => {
      const codePoint = character.codePointAt(0);
      return codePoint >= 0x20 && !(codePoint >= 0x7f && codePoint <= 0x9f);
    });
}

function isReservedComponent(text) {
  if (text === '.' || text === '..') return true;
  const lower = text.replace(/[ .]+$/g, '').toLowerCase();
  if (lower === '.git') return true;
  const name = lower.split('.')[0];
  return ['con', 'prn', 'aux', 'nul', 'clock$', 'conin$', 'conout$'].includes(name)
    || /^(com|lpt)[1-9]$/.test(name);
}

function encodeOrnaKeyComponent(text) {
  const bytes = Buffer.from(text, 'utf8');
  const reserved = isReservedComponent(text);
  const lastNonDot = Array.from(bytes).findLastIndex((byte) => byte !== 0x2e);
  const trailing = lastNonDot + 1;
  let encoded = '';
  for (let index = 0; index < bytes.length; index += 1) {
    const byte = bytes[index];
    const allowed = (byte >= 0x41 && byte <= 0x5a)
      || (byte >= 0x61 && byte <= 0x7a)
      || (byte >= 0x30 && byte <= 0x39)
      || byte === 0x2e
      || byte === 0x5f
      || byte === 0x2d;
    const force = (reserved && index === 0) || (byte === 0x2e && index >= trailing);
    encoded += !force && allowed ? String.fromCharCode(byte) : `~${byte.toString(16).padStart(2, '0')}`;
  }
  return encoded || '~ff';
}

function ornaString(value) {
  return `"${value.replaceAll('\\', '\\\\').replaceAll('"', '\\"').replaceAll('\n', '\\n').replaceAll('\r', '\\r').replaceAll('\t', '\\t')}"`;
}

async function listFiles(directory, relative = '') {
  const entries = await readdir(directory, { withFileTypes: true });
  const files = [];
  for (const entry of entries) {
    const childRelative = relative ? `${relative}/${entry.name}` : entry.name;
    const childPath = path.join(directory, entry.name);
    if (entry.isDirectory()) {
      files.push(...await listFiles(childPath, childRelative));
    } else if (entry.isFile()) {
      files.push(childRelative);
    } else {
      throw new Error(`unsupported non-file browser build entry: ${childRelative}`);
    }
  }
  return files;
}

function rowPathForKey(key) {
  if (!isSafeKey(key)) throw new Error(`unsupported browser asset path: ${key}`);
  const encoded = encodeOrnaKeyComponent(key);
  if (key.length > 200 || encoded.length > 200) throw new Error(`browser asset path is too long: ${key}`);
  return path.join(recordsRoot, `${encoded}.orna`);
}

async function buildExpectedRows() {
  const files = (await listFiles(distributionRoot)).sort();
  if (files.length === 0) throw new Error('browser build is empty; run npm run build first');
  if (files.length > maxAssetRows) throw new Error(`browser build exceeds ${maxAssetRows} asset rows`);

  const rows = new Map();
  for (const relativePath of files) {
    if (!isSafeKey(relativePath)) throw new Error(`unsupported browser asset path: ${relativePath}`);
    const bytes = await readFile(path.join(distributionRoot, relativePath));
    if (bytes.length > maxAssetBytes) throw new Error(`browser asset exceeds ${maxAssetBytes} bytes: ${relativePath}`);
    const extension = path.extname(relativePath).toLowerCase();
    const mediaType = mimeTypes.get(extension) ?? 'application/octet-stream';
    const record = [
      '{',
      `  path: ${ornaString(relativePath)},`,
      `  media_type: ${ornaString(mediaType)},`,
      `  bytes_base64: ${ornaString(bytes.toString('base64'))},`,
      '}',
      '',
    ].join('\n');
    rows.set(rowPathForKey(relativePath), record);
  }
  return rows;
}

async function existingRows() {
  try {
    return (await listFiles(recordsRoot)).map((relative) => path.join(recordsRoot, relative));
  } catch (error) {
    if (error.code === 'ENOENT') return [];
    throw error;
  }
}

try {
  const arguments_ = process.argv.slice(2);
  if (arguments_.length > 1 || arguments_.some((argument) => argument !== '--check')) {
    throw new Error('usage: node scripts/db-assets.mjs [--check]');
  }
  const expected = await buildExpectedRows();
  const existing = await existingRows();
  const unsupported = existing.filter((file) => !file.endsWith('.orna'));
  if (unsupported.length > 0) {
    throw new Error(`refusing to manage non-row Asset file: ${path.relative(repositoryRoot, unsupported[0])}`);
  }
  const stale = existing.filter((file) => !expected.has(file));
  const changed = [];
  for (const [file, expectedContent] of expected) {
    let current;
    try {
      current = await readFile(file, 'utf8');
    } catch (error) {
      if (error.code !== 'ENOENT') throw error;
    }
    if (current !== expectedContent) changed.push([file, expectedContent]);
  }

  if (checkOnly) {
    if (stale.length > 0 || changed.length > 0) {
      for (const file of stale) process.stderr.write(`stale Asset row: ${path.relative(repositoryRoot, file)}\n`);
      for (const [file] of changed) process.stderr.write(`missing or stale Asset row: ${path.relative(repositoryRoot, file)}\n`);
      process.exitCode = 1;
    } else {
      process.stdout.write(`verified ${expected.size} committed playground.Asset rows\n`);
    }
  } else {
    for (const [file, content] of changed) {
      await mkdir(path.dirname(file), { recursive: true });
      await writeFile(file, content, 'utf8');
    }
    for (const file of stale) {
      await rm(file);
    }
    process.stdout.write(`synchronized ${expected.size} playground.Asset rows\n`);
  }
} catch (error) {
  process.stderr.write(`${error.message}\n`);
  process.exitCode = 1;
}
