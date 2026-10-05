import assert from 'node:assert/strict';
import { Worker } from 'node:worker_threads';

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
await lsp.default({ module_or_path: await wasmResponse.arrayBuffer() });

const source = [
  'use std.math.{clamp, increment};',
  'pub fn incremental(value: Int): Int = value + 2;',
  'pub fn exercise(value: Int): Int {',
  '    let next = increment(value);',
  '    next',
  '}',
  'pub fn shadowed(increment: Int): Int = increment;',
  'pub fn qualified_increment(value: Int): Int = std.math.increment(value);',
  'pub fn bounded(value: Int, lower: Int, upper: Int): Int = clamp(value, lower, upper);',
  'pub fn qualified_bounded(value: Int, lower: Int, upper: Int): Int = std.math.clamp(value, lower, upper);',
].join('\n');
function positionAt(offset, document = source) {
  const prefix = document.slice(0, offset);
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

const definition = JSON.parse(lsp.definition(
  source,
  hoverPosition.line,
  hoverPosition.character,
));
assert.equal(definition.uri, 'orna-stdlib:///std/math.orna');
const standardSource = await get('/playground/assets/stdlib/std/math.orna');
assert.match(standardSource.headers.get('content-type') ?? '', /text\/plain/);
const standardText = await standardSource.text();
assert.match(standardText, /pub fn increment\(value: Int\)/);
const mathSymbols = JSON.parse(lsp.document_symbols(standardText));
assert.ok(mathSymbols.some(({ name, kind }) => name === 'increment' && kind === 12));
assert.ok(mathSymbols.some(({ name, kind }) => name === 'clamp' && kind === 12));
for (const symbol of mathSymbols) {
  const symbolLine = standardText.split(/\r?\n/)[symbol.selectionRange.start.line];
  assert.equal(
    symbolLine.slice(symbol.selectionRange.start.character, symbol.selectionRange.end.character),
    symbol.name,
    `standard document symbol range for ${symbol.name}`,
  );
}
const nestedSource = await get('/playground/assets/stdlib/std/text/lines.orna');
assert.match(nestedSource.headers.get('content-type') ?? '', /text\/plain/);
const nestedSymbols = JSON.parse(lsp.document_symbols(await nestedSource.text()));
assert.deepEqual(nestedSymbols.map(({ name }) => name), ['split', 'join', 'count', 'normalise']);
const declarationLine = standardText.split(/\r?\n/)[definition.range.start.line];
assert.equal(
  declarationLine.slice(definition.range.start.character, definition.range.end.character),
  'increment',
);

const qualifiedNavigationOffset = source.indexOf('std.math.increment(value)')
  + 'std.math.increment'.length - 1;
const qualifiedPosition = positionAt(qualifiedNavigationOffset);
const qualifiedDefinition = JSON.parse(lsp.definition(
  source,
  qualifiedPosition.line,
  qualifiedPosition.character,
));
assert.equal(qualifiedDefinition.uri, 'orna-stdlib:///std/math.orna');
assert.deepEqual(qualifiedDefinition.range, definition.range);

const references = JSON.parse(lsp.references(
  source,
  hoverPosition.line,
  hoverPosition.character,
  true,
));
assert.equal(references.length, 4, 'standard declaration, import, and two calls');
assert.equal(references.filter(({ uri }) => uri === 'orna-stdlib:///std/math.orna').length, 1);
assert.equal(references.filter(({ uri }) => uri === 'file:///playground/main.orna').length, 3);
const qualifiedReferences = JSON.parse(lsp.references(
  source,
  qualifiedPosition.line,
  qualifiedPosition.character,
  true,
));
assert.deepEqual(qualifiedReferences, references);

const shadowOffset = source.lastIndexOf('increment;') + 1;
const shadowPosition = positionAt(shadowOffset);
const shadowDefinition = JSON.parse(lsp.definition(
  source,
  shadowPosition.line,
  shadowPosition.character,
));
assert.equal(shadowDefinition.uri, 'file:///playground/main.orna');
const shadowReferences = JSON.parse(lsp.references(
  source,
  shadowPosition.line,
  shadowPosition.character,
  true,
));
assert.equal(shadowReferences.length, 2, 'local parameter declaration and use');
assert.ok(shadowReferences.every(({ uri }) => uri === 'file:///playground/main.orna'));

const clampOffset = source.indexOf('clamp(value, lower, upper)');
assert.notEqual(clampOffset, -1);
const signaturePosition = positionAt(clampOffset + 'clamp(value, lower, '.length);
const signature = JSON.parse(lsp.signature_help(
  source,
  signaturePosition.line,
  signaturePosition.character,
));
assert.equal(signature.activeSignature, 0);
assert.equal(signature.activeParameter, 2);
assert.match(signature.signatures[0].label, /pub fn clamp\(value: Int, lower: Int, upper: Int\): Int/);
assert.match(JSON.stringify(signature.signatures[0].documentation), /inclusive interval/);
assert.deepEqual(signature.signatures[0].parameters.map(({ label }) => label), [
  'value: Int',
  'lower: Int',
  'upper: Int',
]);

const lastLine = source.split('\n').at(-1);
const inlayHints = JSON.parse(lsp.inlay_hints(
  source,
  0,
  0,
  source.split('\n').length - 1,
  [...lastLine].length,
));
const inlayLabels = inlayHints.map(({ label }) => label);
assert.ok(inlayLabels.includes(': Int'), 'standard return type should infer a local type hint');
assert.ok(inlayLabels.includes('value: '), 'standard call should show its parameter name');
assert.ok(inlayLabels.includes('lower: '), 'standard call should show its lower bound name');
assert.ok(inlayLabels.includes('upper: '), 'standard call should show its upper bound name');

const qualifiedOffset = source.indexOf('std.math.clamp(value, lower, upper)');
assert.notEqual(qualifiedOffset, -1);
const qualifiedEnd = qualifiedOffset + 'std.math.clamp(value, lower, upper)'.length;
const qualifiedStartPosition = positionAt(qualifiedOffset);
const qualifiedEndPosition = positionAt(qualifiedEnd);
const qualifiedHints = JSON.parse(lsp.inlay_hints(
  source,
  qualifiedStartPosition.line,
  qualifiedStartPosition.character,
  qualifiedEndPosition.line,
  qualifiedEndPosition.character,
));
assert.deepEqual(qualifiedHints.map(({ label }) => label), ['value: ', 'lower: ', 'upper: ']);

const importedSessionSource = [
  'use std.math.{clamp};',
  'pub fn imported(sample: Int, floor: Int, ceiling: Int): Int = clamp(sample, floor, ceiling);',
].join('\n');
const qualifiedSessionSource = [
  'pub fn qualified(number: Int): Int =',
  '    std.math.increment(number);',
].join('\n');
function runServedInlaySession(client, document) {
  return new Promise((resolve, reject) => {
    const worker = new Worker(new URL('./prove-served-inlay-session.mjs', import.meta.url), {
      workerData: { baseUrl, client, document },
    });
    worker.once('message', (result) => {
      if (result.error) reject(new Error(result.error));
      else resolve(result);
    });
    worker.once('error', reject);
    worker.once('exit', (code) => {
      if (code !== 0) reject(new Error(`served inlay worker ${client} exited with ${code}`));
    });
  });
}

const [importedSession, qualifiedSession] = await Promise.all([
  runServedInlaySession('imported', importedSessionSource),
  runServedInlaySession('qualified', qualifiedSessionSource),
]);
assert.deepEqual(
  importedSession.hints.map(({ label }) => label),
  ['value: ', 'lower: ', 'upper: '],
  'imported session should receive its own standard parameter hints',
);
assert.deepEqual(
  qualifiedSession.hints.map(({ label }) => label),
  ['value: '],
  'qualified session should receive only its own increment parameter hint',
);
const importedCallStart = importedSessionSource.indexOf('clamp(sample, floor, ceiling)');
const qualifiedCallStart = qualifiedSessionSource.indexOf('std.math.increment(number)');
assert.notEqual(importedCallStart, -1);
assert.notEqual(qualifiedCallStart, -1);
assert.deepEqual(
  importedSession.hints.map(({ position }) => position),
  [
    positionAt(importedCallStart + 'clamp('.length, importedSessionSource),
    positionAt(importedCallStart + 'clamp(sample, '.length, importedSessionSource),
    positionAt(importedCallStart + 'clamp(sample, floor, '.length, importedSessionSource),
  ],
  'imported hint positions should belong to the imported document',
);
assert.deepEqual(
  qualifiedSession.hints.map(({ position }) => position),
  [positionAt(qualifiedCallStart + 'std.math.increment('.length, qualifiedSessionSource)],
  'qualified hint positions should belong to the qualified document',
);

console.log('served WASM completion order: increment, incremental; exact match preselected');
console.log('served WASM hover: std.math.increment signature and documentation verified');
console.log('served WASM signature help: std.math.clamp active upper parameter and hints verified');
console.log('served WASM inlay hints: imported and qualified std calls plus inferred result type verified');
console.log('served WASM definition/references: DB standard source, imported and qualified calls, and local shadowing verified');
console.log('served WASM document symbols: root and nested DB standard modules have exact symbol ranges');
console.log('concurrent served WASM inlay sessions: independent labels and source positions verified');
