import assert from "node:assert/strict";
import { executeBrowserProfile } from "../src/browser-profile-host.mjs";

// ORNA-APP-016: the browser host JS may bind only the raw profile ABI and the
// instance memory. It must not reach server handles or closures, and it must not
// open any network channel of its own.
const PLATFORM_SURFACE = new Set([
  "memory",
  "orna_browser_profile_alloc",
  "orna_browser_profile_dealloc",
  "orna_browser_profile_execute",
]);

const encoder = new TextEncoder();
const decoder = new TextDecoder();
const memory = new WebAssembly.Memory({ initial: 1 });
const readBytes = (pointer, length) => new Uint8Array(memory.buffer, pointer, length).slice();
let nextPointer = 64;

// Stub evaluator: the surface under test is the host's marshalling and the set of
// instance exports it touches, not the evaluator. It answers "<entry>:<source length>".
const serverAuthority = () => {
  throw new Error("server authority reached the browser");
};
const exports = {
  memory,
  orna_browser_profile_alloc(length) {
    const pointer = nextPointer;
    nextPointer += length + 8;
    return pointer;
  },
  orna_browser_profile_dealloc() {},
  orna_browser_profile_execute(sourcePointer, sourceLength, entryPointer, entryLength,
    outputPointer, outputCapacity, outputLengthPointer) {
    const entry = decoder.decode(readBytes(entryPointer, entryLength));
    if (entry !== "client" && entry !== "render") return 3;
    const result = encoder.encode(`${entry}:${sourceLength}`);
    const view = new DataView(memory.buffer);
    view.setUint32(outputLengthPointer, result.length, true);
    if (outputCapacity < result.length) return 4;
    new Uint8Array(memory.buffer, outputPointer, result.length).set(result);
    return 0;
  },
  orna_server_handle: serverAuthority,
  orna_server_closure: serverAuthority,
};

const touched = new Set();
const instance = {
  exports: new Proxy(exports, {
    get(target, name) {
      touched.add(String(name));
      return Reflect.get(target, name);
    },
  }),
};

const networkGlobals = ["fetch", "WebSocket", "EventSource"];
const saved = Object.fromEntries(networkGlobals.map((name) => [name, globalThis[name]]));
for (const name of networkGlobals) {
  Object.defineProperty(globalThis, name, {
    configurable: true,
    value() { throw new Error(`browser host JS opened a network channel: ${name}`); },
  });
}

try {
  const output = executeBrowserProfile(instance, "fn client() {}", "client");
  assert.equal(decoder.decode(output), `client:${encoder.encode("fn client() {}").length}`);

  for (const name of touched) {
    assert.ok(PLATFORM_SURFACE.has(name), `browser host JS touched a non-platform export: ${name}`);
  }
  for (const name of PLATFORM_SURFACE) {
    assert.ok(touched.has(name), `browser host JS never used the platform export: ${name}`);
  }

  assert.throws(
    () => executeBrowserProfile(instance, "fn client() {}", "missing"),
    /profile Wasm execution failed with status 3/,
    "an unknown entry must be rejected, not answered with a fallback result",
  );
} finally {
  for (const name of networkGlobals) {
    Object.defineProperty(globalThis, name, { configurable: true, writable: true, value: saved[name] });
  }
}

console.log("browser host JS binds only the platform profile surface");
