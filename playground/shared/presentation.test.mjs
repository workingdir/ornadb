import assert from 'node:assert/strict';
import test from 'node:test';

import {
  LivePresentation,
  PresentationError,
  applyPatches,
  decodeCbor,
  decodeEnvelope,
  encodeCbor,
  runResultFromPresentation,
} from './presentation.mjs';

const WATCH = new Uint8Array(16).fill(7);
const REQUEST = new Uint8Array(16).fill(1);

function present(kind = 'text', key = null, properties = new Map(), children = []) {
  return { tag: 60012n, value: [kind, key, properties, children] };
}

function snapshot(revision, node, watch = WATCH, request = REQUEST) {
  return {
    code: 16,
    request,
    watch,
    body: new Map([[0n, BigInt(revision)], [1n, node], [2n, null]]),
  };
}

function delta(base, next, patches, watch = WATCH) {
  return {
    code: 17,
    request: null,
    watch,
    body: new Map([[0n, BigInt(base)], [1n, BigInt(next)], [2n, patches], [3n, null]]),
  };
}

test('accepts a matching snapshot and applies property deltas in revision order', () => {
  const owner = new LivePresentation();
  const initial = present('text', null, new Map([['value', 'before']]));
  assert.equal(owner.receive(snapshot(3, initial)).type, 'snapshot');

  const update = owner.receive(delta(3, 4, [[2n, [[0n, 'value']], 'after']]));
  assert.equal(update.type, 'delta');
  assert.equal(update.current.revision, 4n);
  assert.equal(update.current.present.value[2].get('value'), 'after');
  assert.equal(initial.value[2].get('value'), 'before');
});

test('supports keyed child add, replace, remove, and move patches', () => {
  const first = present('text', [3n, 'first'], new Map([['value', 'one']]));
  const second = present('text', [3n, 'second'], new Map([['value', 'two']]));
  const root = present('group', null, new Map(), [first, second]);
  const inserted = present('text', [3n, 'third'], new Map([['value', 'three']]));

  const result = applyPatches(root, [
    [0n, [[3n, 'third']], inserted],
    [2n, [[3n, 'first'], [0n, 'value']], 'updated'],
    [3n, [[3n, 'second']], [[2n, 0n]]],
    [1n, [[3n, 'third']]],
  ]);

  assert.deepEqual(result.value[3].map(child => child.value[1]), [[3n, 'second'], [3n, 'first']]);
  assert.equal(result.value[3][1].value[2].get('value'), 'updated');
  assert.equal(root.value[3].length, 2);
});

test('a failed later patch leaves the published tree unchanged and requests a snapshot', () => {
  const owner = new LivePresentation();
  const original = present('text', null, new Map([['value', 'stable']]));
  owner.receive(snapshot(8, original));

  const result = owner.receive(delta(8, 9, [
    [2n, [[0n, 'value']], 'candidate'],
    [1n, [[0n, 'missing']]],
  ]));

  assert.equal(result.type, 'resync');
  assert.equal(result.shouldSend, true);
  assert.equal(result.current.revision, 8n);
  assert.equal(result.current.present.value[2].get('value'), 'stable');
  assert.equal(owner.receive(delta(8, 10, [])).shouldSend, false);
  assert.equal(owner.awaitingSnapshot, true);
});

test('validates each intermediate tree and rejects duplicate child identities', () => {
  const root = present('group', null, new Map([['selected', 'value']]), [
    present('text', [3n, 'same']),
  ]);
  assert.throws(() => applyPatches(root, [
    [2n, [[0n, 'selected']], { tag: 60012n, value: [] }],
    [2n, [[0n, 'selected']], 'repaired later'],
  ]), PresentationError);
  assert.throws(() => applyPatches(root, [
    [0n, [[2n, 0n]], present('text', [3n, 'same'])],
  ]), PresentationError);
  assert.equal(root.value[2].get('selected'), 'value');
});

test('rejects revision gaps, older snapshots, conflicting equal snapshots, and foreign watches', () => {
  const owner = new LivePresentation();
  owner.receive(snapshot(9, present()));

  assert.equal(owner.receive(delta(8, 10, [])).type, 'resync');
  assert.equal(owner.current.revision, 9n);
  owner.receive(snapshot(10, present()));
  assert.equal(owner.receive(snapshot(9, present())).type, 'resync');
  assert.equal(owner.current.revision, 10n);

  const conflict = present('group');
  assert.equal(owner.receive(snapshot(10, conflict)).type, 'resync');
  assert.equal(owner.receive(delta(10, 11, [], new Uint8Array(16).fill(8))).type, 'ignored');
});

test('decodes canonical envelopes and rejects duplicate keys, trailing data, and non-minimal integers', () => {
  const bytes = encodeCbor(new Map([
    [0n, 1n], [1n, 16n], [2n, REQUEST], [3n, WATCH],
    [4n, new Map([[0n, 2n], [1n, present()], [2n, null]])],
  ]));
  assert.equal(decodeEnvelope(bytes).code, 16);
  assert.throws(() => decodeCbor(Uint8Array.of(0xa2, 0x00, 0x01, 0x00, 0x02)), PresentationError);
  assert.throws(() => decodeCbor(Uint8Array.of(0x18, 0x01)), PresentationError);
  assert.throws(() => decodeCbor(Uint8Array.of(0x01, 0x00)), PresentationError);
  const mandatoryExtension = encodeCbor(new Map([
    [0n, 1n], [1n, 16n], [2n, REQUEST], [3n, WATCH],
    [4n, new Map([[0n, 2n], [1n, present()], [2n, null], [32768n, true]])],
  ]));
  assert.throws(() => decodeEnvelope(mandatoryExtension), PresentationError);
});

test('rejects oversized and malformed patch payloads before publishing', () => {
  assert.throws(() => applyPatches(present(), [[0n, [], present()]]), PresentationError);
  const huge = new Uint8Array(16 * 1024 * 1024 + 1);
  assert.throws(() => decodeCbor(huge), PresentationError);
});

test('extracts successful and failed run summaries from a validated presentation tree', () => {
  const successful = present('run.events', null, new Map(), [present('run', null, new Map([
    ['status', 'success'], ['value', 'done'], ['stdout', 'hello'],
  ]))]);
  assert.deepEqual(runResultFromPresentation(successful), {
    ok: true,
    values: ['done'],
    stdout: 'hello',
    errors: [],
  });

  const failed = present('run.events', null, new Map(), [present('run', null, new Map([
    ['status', 'failure'], ['error_code', 'E2200'], ['line', 2n], ['column', 4n],
  ]))]);
  assert.deepEqual(runResultFromPresentation(failed), {
    ok: false,
    values: [],
    stdout: '',
    errors: [{ message: 'E2200', line: 2, col: 4 }],
  });
  assert.equal(runResultFromPresentation(present()), null);
});
