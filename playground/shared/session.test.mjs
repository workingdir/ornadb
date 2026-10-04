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
