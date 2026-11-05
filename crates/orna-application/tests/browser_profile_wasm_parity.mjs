import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { executeBrowserProfile } from "../src/browser-profile-host.mjs";

const [wasmPath, sourcePath, entry, expectedHex] = process.argv.slice(2);
assert.ok(wasmPath && sourcePath && entry && expectedHex, "usage: node browser_profile_wasm_parity.mjs <wasm> <source> <entry> <expected-hex>");
const wasmBytes = await readFile(wasmPath);
const imports = {
  __wbindgen_placeholder__: {
    __wbindgen_describe() {},
    __wbg___wbindgen_throw_41e9ee4f547fc59a() { throw new Error("Wasm panic"); },
  },
  __wbindgen_externref_xform__: {
    __wbindgen_externref_table_grow() { return 0; },
    __wbindgen_externref_table_set_null() {},
  },
};
const { instance } = await WebAssembly.instantiate(wasmBytes, imports);
const source = await readFile(sourcePath, "utf8");
const result = executeBrowserProfile(instance, source, entry);
assert.equal(Buffer.from(result).toString("hex"), expectedHex, "Wasm evaluator output must equal native evaluator canonical OVB bytes");
console.log("shared evaluator Wasm output matches native OVB bytes");
