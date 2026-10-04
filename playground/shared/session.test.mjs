import assert from 'node:assert/strict';
import test from 'node:test';

import {
  LiveSession,
  decodeEnvelope,
  encodeCbor,
  envelope,
} from './presentation.mjs';

const DATABASE = '01010101-0101-0101-0101-010101010101';
const WATCH = new Uint8Array(16).fill(9);

class FakeSocket {
  static latest;

  constructor(url, protocol) {
    this.url = String(url);
    this.protocol = protocol;
    this.readyState = 0;
    this.listeners = new Map();
    this.sent = [];
    FakeSocket.latest = this;
    queueMicrotask(() => {
      this.readyState = 1;
      this.#emit('open');
    });
  }

  addEventListener(type, listener, options = {}) {
    const listeners = this.listeners.get(type) ?? [];
    listeners.push({ listener, once: options.once === true });
    this.listeners.set(type, listeners);
  }

  send(bytes) {
    this.sent.push(bytes);
  }

  close() {
    this.readyState = 3;
    this.#emit('close');
  }

  receive(frame) {
    const bytes = encodeCbor(frame);
    const data = bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength);
    this.#emit('message', { data });
  }

  #emit(type, event = {}) {
    const listeners = this.listeners.get(type) ?? [];
    this.listeners.set(type, listeners.filter(entry => !entry.once));
    for (const { listener } of listeners) listener(event);
  }
}

test('connects to the same-origin live endpoint and requests resync on a revision gap', async () => {
  const fetches = [];
  const publications = [];
  const statuses = [];
  const fetcher = async (url, options) => {
    fetches.push({ url: String(url), options });
    if (options.method === 'DELETE') return { ok: true };
    return {
      ok: true,
      async json() {
        return {
          session: DATABASE,
          database: DATABASE,
          resume_token: 'a'.repeat(43),
          websocket_path: '/orna/live/01010101-0101-0101-0101-010101010101',
        };
      },
    };
  };
  const live = new LiveSession(DATABASE, {
    pageUrl: 'http://127.0.0.1:8756/playground/',
    fetcher,
    WebSocketConstructor: FakeSocket,
    onPresentation: (current, kind) => publications.push({ current, kind }),
    onStatus: message => statuses.push(message),
  });

  await live.connect();
  const socket = FakeSocket.latest;
  assert.equal(socket.protocol, 'orna.present.v1');
  assert.equal(socket.url, 'ws://127.0.0.1:8756/orna/live/01010101-0101-0101-0101-010101010101');
  assert.equal(fetches[0].url, 'http://127.0.0.1:8756/orna/session');

  socket.receive(envelope(16, new Uint8Array(16).fill(1), WATCH, new Map([
    [0n, 5n],
    [1n, { tag: 60012n, value: ['text', null, new Map([['value', 'current']]), []] }],
    [2n, null],
  ])));
  assert.equal(publications[0].kind, 'snapshot');
  assert.equal(live.presentation.current.revision, 5n);

  socket.receive(envelope(17, null, WATCH, new Map([
    [0n, 4n], [1n, 6n], [2n, []], [3n, null],
  ])));
  assert.equal(live.presentation.current.revision, 5n);
  assert.equal(socket.sent.length, 1);
  assert.equal(decodeEnvelope(socket.sent[0]).code, 2);
  assert.ok(statuses.includes('Presentation needs a fresh snapshot.'));

  live.dispose();
  await new Promise(resolve => setTimeout(resolve, 0));
  assert.equal(fetches[1].options.method, 'DELETE');
  assert.equal(fetches[1].url, 'http://127.0.0.1:8756/orna/session/01010101-0101-0101-0101-010101010101');
  assert.equal(socket.readyState, 3);
});

test('isolates watched presentations and resolves explicit refreshes from a fresh snapshot', async () => {
  const fetcher = async (_url, options) => options.method === 'DELETE'
    ? { ok: true }
    : {
      ok: true,
      async json() {
        return {
          session: DATABASE,
          database: DATABASE,
          resume_token: 'b'.repeat(43),
          websocket_path: '/orna/live/01010101-0101-0101-0101-010101010101',
        };
      },
    };
  const live = new LiveSession(DATABASE, {
    pageUrl: 'http://127.0.0.1:8756/playground/',
    fetcher,
    WebSocketConstructor: FakeSocket,
  });
  await live.connect();
  const socket = FakeSocket.latest;
  socket.receive(envelope(16, new Uint8Array(16).fill(1), WATCH, new Map([
    [0n, 1n],
    [1n, { tag: 60012n, value: ['text', null, new Map(), []] }],
    [2n, null],
  ])));

  const watched = live.watch('run.events');
  const request = decodeEnvelope(socket.sent[0]);
  assert.equal(request.code, 5);
  assert.equal(request.body.get(0n), 'run.events');
  const eventWatch = new Uint8Array(16).fill(8);
  socket.receive(envelope(16, request.request, eventWatch, new Map([
    [0n, 4n],
    [1n, { tag: 60012n, value: ['run.events', null, new Map(), []] }],
    [2n, null],
  ])));
  const eventState = await watched;
  assert.equal(eventState.presentation.current.revision, 4n);
  assert.equal(live.presentation.current.revision, 1n);

  const refreshing = live.refresh(eventState);
  const resync = decodeEnvelope(socket.sent[1]);
  assert.equal(resync.code, 2);
  assert.deepEqual(resync.watch, eventWatch);
  socket.receive(envelope(16, resync.request, eventWatch, new Map([
    [0n, 5n],
    [1n, { tag: 60012n, value: ['run.events', null, new Map(), []] }],
    [2n, null],
  ])));
  const current = await refreshing;
  assert.equal(current.revision, 5n);
  assert.equal(live.presentation.current.revision, 1n);
  live.dispose();
});

test('reconciles typed deltas and resync independently across live sessions', async () => {
  const sessionIds = [
    '02020202-0202-0202-0202-020202020202',
    '03030303-0303-0303-0303-030303030303',
  ];
  const fetcher = async (_url, options) => {
    if (options.method === 'DELETE') return { ok: true };
    const session = sessionIds.shift();
    return {
      ok: true,
      async json() {
        return {
          session,
          database: DATABASE,
          resume_token: 'c'.repeat(43),
          websocket_path: `/orna/live/${session}`,
        };
      },
    };
  };
  const makeSession = () => new LiveSession(DATABASE, {
    pageUrl: 'http://127.0.0.1:8756/playground/',
    fetcher,
    WebSocketConstructor: FakeSocket,
  });
  const first = makeSession();
  await first.connect();
  const firstSocket = FakeSocket.latest;
  const second = makeSession();
  await second.connect();
  const secondSocket = FakeSocket.latest;

  const firstPending = first.watch('customer');
  const firstWatchRequest = decodeEnvelope(firstSocket.sent[0]);
  const secondPending = second.watch('customer');
  const secondWatchRequest = decodeEnvelope(secondSocket.sent[0]);
  const firstWatch = new Uint8Array(16).fill(7);
  const secondWatch = new Uint8Array(16).fill(8);
  const typedTree = value => ({
    tag: 60012n,
    value: ['group', null, new Map(), [{
      tag: 60012n,
      value: ['text', [0n, 'customer'], new Map([['name', value]]), []],
    }]],
  });
  const install = (socket, request, watch, present) => socket.receive(envelope(
    16,
    request,
    watch,
    new Map([[0n, 0n], [1n, present], [2n, null]]),
  ));
  const patchName = (socket, watch, base, revision, value) => socket.receive(envelope(
    17,
    null,
    watch,
    new Map([
      [0n, BigInt(base)],
      [1n, BigInt(revision)],
      [2n, [[2n, [[0n, 'customer'], [0n, 'name']], value]]],
      [3n, null],
    ]),
  ));

  install(firstSocket, firstWatchRequest.request, firstWatch, typedTree('first-before'));
  install(secondSocket, secondWatchRequest.request, secondWatch, typedTree('second-before'));
  const firstState = await firstPending;
  const secondState = await secondPending;

  patchName(firstSocket, firstWatch, 0, 1, 'first-after');
  assert.equal(firstState.presentation.current.revision, 1n);
  assert.equal(firstState.presentation.current.present.value[3][0].value[2].get('name'), 'first-after');
  assert.equal(secondState.presentation.current.revision, 0n);
  assert.equal(secondState.presentation.current.present.value[3][0].value[2].get('name'), 'second-before');

  patchName(secondSocket, secondWatch, 0, 1, 'second-after');
  assert.equal(secondState.presentation.current.revision, 1n);
  assert.equal(secondState.presentation.current.present.value[3][0].value[2].get('name'), 'second-after');
  assert.equal(firstState.presentation.current.present.value[3][0].value[2].get('name'), 'first-after');

  patchName(firstSocket, firstWatch, 0, 2, 'stale-gap');
  assert.equal(firstState.presentation.current.revision, 1n);
  assert.equal(firstState.presentation.current.present.value[3][0].value[2].get('name'), 'first-after');
  const resync = decodeEnvelope(firstSocket.sent[1]);
  assert.equal(resync.code, 2);
  assert.deepEqual(resync.watch, firstWatch);
  assert.equal(secondSocket.sent.length, 1);

  firstSocket.receive(envelope(16, resync.request, firstWatch, new Map([
    [0n, 3n], [1n, typedTree('first-recovered')], [2n, null],
  ])));
  assert.equal(firstState.presentation.current.revision, 3n);
  assert.equal(firstState.presentation.current.present.value[3][0].value[2].get('name'), 'first-recovered');
  assert.equal(secondState.presentation.current.revision, 1n);
  assert.equal(secondState.presentation.current.present.value[3][0].value[2].get('name'), 'second-after');

  first.dispose();
  second.dispose();
});
