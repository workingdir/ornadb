import assert from 'node:assert/strict';

const baseUrl = process.argv[2]?.replace(/\/$/, '');
assert.ok(baseUrl, 'pass the base URL of an active `orna serve` process');

async function get(path) {
  const response = await fetch(`${baseUrl}${path}`);
  assert.equal(response.status, 200, `GET ${path}`);
  return response;
}

const shell = await get('/playground/');
assert.match(await shell.text(), /Orna playground/);

const [bindingResponse, wasmResponse] = await Promise.all([
  get('/playground/assets/lsp-wasm/orna_lsp.js'),
  get('/playground/assets/lsp-wasm/orna_lsp_bg.wasm'),
]);
assert.match(bindingResponse.headers.get('content-type') ?? '', /javascript/);
assert.match(wasmResponse.headers.get('content-type') ?? '', /application\/wasm/);

globalThis.self ??= globalThis;
const bindingSource = await bindingResponse.text();
const bindingUrl = `data:text/javascript;base64,${Buffer.from(bindingSource).toString('base64')}`;
const lsp = await import(bindingUrl);
await lsp.default(await wasmResponse.arrayBuffer());

const source = [
  'use std.math.{increment};',
  'pub fn incremental(value: Int): Int = value + 2;',
  'pub fn exercise(value: Int): Int = increment(value);',
].join('\n');
function positionAt(offset) {
  const prefix = source.slice(0, offset);
  const line = prefix.split('\n').length - 1;
  const lineStart = prefix.lastIndexOf('\n') + 1;
  return { line, character: [...prefix.slice(lineStart)].length };
}

const callOffset = source.indexOf('increment(value)');
assert.notEqual(callOffset, -1);
const completionPosition = positionAt(callOffset + 'increment'.length);
const completions = JSON.parse(lsp.completions(
  source,
  completionPosition.line,
  completionPosition.character,
));
assert.deepEqual(completions.slice(0, 2).map(({ label }) => label), ['increment', 'incremental']);
assert.equal(completions[0].preselect, true);
assert.equal(completions[0].sortText, '0-1-increment');

const hoverPosition = positionAt(callOffset + 'increment'.length - 1);
const hover = JSON.parse(lsp.hover(source, hoverPosition.line, hoverPosition.character));
const hoverText = JSON.stringify(hover);
assert.match(hoverText, /fn increment\(value: Int\): Int/);
assert.match(hoverText, /exact successor/);

console.log('served WASM completion order: increment, incremental; exact match preselected');
console.log('served WASM hover: std.math.increment signature and documentation verified');
