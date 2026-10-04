import assert from 'node:assert/strict';
import { access, readdir, readFile } from 'node:fs/promises';
import { join } from 'node:path';

const [siteDirectory, repositoryName, pagesUrl] = process.argv.slice(2);
assert(repositoryName, 'Pass the GitHub repository name as the second argument.');
assert(siteDirectory || pagesUrl, 'Pass a local Pages artifact directory or a published Pages URL.');

const pageRoot = pagesUrl ? new URL(pagesUrl) : undefined;
if (pageRoot && !pageRoot.pathname.endsWith('/')) pageRoot.pathname += '/';

const channels = [
  { name: 'stable', directory: '', url: './', base: `/${repositoryName}/` },
  { name: 'dev', directory: 'dev', url: 'dev/', base: `/${repositoryName}/dev/` },
];

function pageAssets(html) {
  const tags = html.matchAll(/<(?:script|link)\b[^>]*>/gi);
  return [...tags].flatMap(([tag]) => {
    const match = tag.match(/\b(?:src|href)="([^"]+)"/i);
    return match ? [match[1]] : [];
  });
}

function assertPlaygroundPage(html, channelName) {
  assert.match(html, /<title>Orna playground<\/title>/, `${channelName} page has the playground title`);
  const assets = pageAssets(html);
  assert(assets.some((asset) => asset.endsWith('.js')), `${channelName} page references JavaScript`);
  assert(assets.some((asset) => asset.endsWith('.css')), `${channelName} page references CSS`);
  return assets;
}

function assertEmbedEntry(source, channelName) {
  assert.match(source, /orna-playground-embed-loader\/v1/, `${channelName} has the embed entry marker`);
  assert.match(source, /data-target/, `${channelName} embed entry accepts a target element`);
  assert.match(source, /data-src/, `${channelName} embed entry accepts a served playground URL`);
}

async function checkLocalChannel(channel) {
  const channelDirectory = join(siteDirectory, channel.directory);
  const html = await readFile(join(channelDirectory, 'index.html'), 'utf8');
  const embedEntry = await readFile(join(channelDirectory, 'embed.js'), 'utf8');
  const assets = assertPlaygroundPage(html, channel.name);
  assertEmbedEntry(embedEntry, channel.name);

  for (const reference of assets) {
    const assetUrl = new URL(reference, 'https://pages-smoke.invalid');
    if (assetUrl.origin !== 'https://pages-smoke.invalid') continue;
    assert(
      assetUrl.pathname.startsWith(channel.base),
      `${channel.name} asset ${reference} uses the expected ${channel.base} base path`,
    );
    const assetPath = decodeURIComponent(assetUrl.pathname.slice(channel.base.length));
    if (assetPath) await access(join(channelDirectory, assetPath));
  }

  const lspFiles = await readdir(join(channelDirectory, 'lsp-wasm'));
  assert(lspFiles.some((file) => file.endsWith('.js')), `${channel.name} contains the LSP JavaScript module`);
  assert(lspFiles.some((file) => file.endsWith('.wasm')), `${channel.name} contains the LSP WebAssembly module`);
  process.stdout.write(`[pages-smoke] ${channel.name}: local index, referenced assets, LSP modules, and embed.js are present\n`);
}

async function fetchPublished(url) {
  let lastResponse;
  for (let attempt = 1; attempt <= 10; attempt += 1) {
    const response = await fetch(url, { signal: AbortSignal.timeout(15_000) });
    if (response.ok) return response;
    lastResponse = response;
    if (attempt < 10) await new Promise((resolve) => setTimeout(resolve, 5_000));
  }
  assert.fail(`HTTP ${lastResponse.status} after 10 attempts: ${url}`);
}

async function checkPublishedChannel(channel) {
  const channelUrl = new URL(channel.url, pageRoot);
  const response = await fetchPublished(channelUrl);
  const assets = assertPlaygroundPage(await response.text(), channel.name);
  const embedResponse = await fetchPublished(new URL('embed.js', channelUrl));
  assertEmbedEntry(await embedResponse.text(), channel.name);
  let jsCount = 0;
  let cssCount = 0;

  for (const reference of assets) {
    const assetUrl = new URL(reference, channelUrl);
    if (assetUrl.origin !== pageRoot.origin) continue;
    await fetchPublished(assetUrl);
    if (assetUrl.pathname.endsWith('.js')) jsCount += 1;
    if (assetUrl.pathname.endsWith('.css')) cssCount += 1;
  }

  assert(jsCount > 0, `${channel.name} published JavaScript assets`);
  assert(cssCount > 0, `${channel.name} published CSS assets`);
  process.stdout.write(`[pages-smoke] ${channel.name}: page, embed.js, and ${jsCount} JavaScript/${cssCount} CSS assets returned HTTP 200\n`);
}

for (const channel of channels) {
  if (siteDirectory) await checkLocalChannel(channel);
  if (pageRoot) await checkPublishedChannel(channel);
}
