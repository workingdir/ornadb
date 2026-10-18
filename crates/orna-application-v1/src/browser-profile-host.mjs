// Host glue for the raw browser-profile Wasm ABI. This file marshals source/result bytes;
// application behavior remains entirely in the Orna module evaluated by Wasm.
export function executeBrowserProfile(instance, source, entry) {
  const { memory, orna_browser_profile_alloc: alloc, orna_browser_profile_dealloc: dealloc,
    orna_browser_profile_execute: execute } = instance.exports;
  if (!(memory instanceof WebAssembly.Memory)) throw new TypeError("profile Wasm must export memory");

  const encoder = new TextEncoder();
  const sourceBytes = encoder.encode(source);
  const entryBytes = encoder.encode(entry);
  const sourcePointer = alloc(sourceBytes.length);
  const entryPointer = alloc(entryBytes.length);
  const outputLengthPointer = alloc(4);
  if (!sourcePointer || !entryPointer || !outputLengthPointer) {
    if (sourcePointer) dealloc(sourcePointer, sourceBytes.length);
    if (entryPointer) dealloc(entryPointer, entryBytes.length);
    if (outputLengthPointer) dealloc(outputLengthPointer, 4);
    throw new RangeError("profile Wasm allocation failed");
  }

  let outputPointer = 0;
  let outputCapacity = 0;
  try {
    new Uint8Array(memory.buffer, sourcePointer, sourceBytes.length).set(sourceBytes);
    new Uint8Array(memory.buffer, entryPointer, entryBytes.length).set(entryBytes);
    let status = execute(sourcePointer, sourceBytes.length, entryPointer, entryBytes.length,
      outputPointer, outputCapacity, outputLengthPointer);
    let outputLength = new DataView(memory.buffer).getUint32(outputLengthPointer, true);
    if (status === 4) {
      outputCapacity = outputLength;
      outputPointer = alloc(outputCapacity);
      if (!outputPointer) throw new RangeError("profile Wasm allocation failed");
      status = execute(sourcePointer, sourceBytes.length, entryPointer, entryBytes.length,
        outputPointer, outputCapacity, outputLengthPointer);
      outputLength = new DataView(memory.buffer).getUint32(outputLengthPointer, true);
    }
    if (status !== 0) throw new Error("profile Wasm execution failed with status " + status);
    if (outputLength > outputCapacity) throw new Error("profile Wasm returned an invalid output length");
    return new Uint8Array(memory.buffer, outputPointer, outputLength).slice();
  } finally {
    dealloc(sourcePointer, sourceBytes.length);
    dealloc(entryPointer, entryBytes.length);
    dealloc(outputLengthPointer, 4);
    if (outputPointer) dealloc(outputPointer, outputCapacity);
  }
}
