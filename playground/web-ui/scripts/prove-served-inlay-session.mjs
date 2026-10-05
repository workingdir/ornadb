import { parentPort, workerData } from 'node:worker_threads';

async function get(path) {
  const response = await fetch(`${workerData.baseUrl}${path}`);
  if (response.status !== 200) throw new Error(`GET ${path} returned ${response.status}`);
  return response;
}

try {
  const shell = await get('/playground/');
  if (!(await shell.text()).includes('Orna playground')) {
    throw new Error('served shell did not contain the Playground entry');
  }
  const [bindingResponse, wasmResponse] = await Promise.all([
    get('/playground/assets/lsp-wasm/orna_lsp.js'),
    get('/playground/assets/lsp-wasm/orna_lsp_bg.wasm'),
  ]);
  if (!/javascript/.test(bindingResponse.headers.get('content-type') ?? '')) {
    throw new Error('served LSP binding did not have a JavaScript media type');
  }
  if (!/application\/wasm/.test(wasmResponse.headers.get('content-type') ?? '')) {
    throw new Error('served LSP package did not have a WebAssembly media type');
  }

  globalThis.self ??= globalThis;
  const bindingSource = await bindingResponse.text();
  const bindingUrl = `data:text/javascript;base64,${Buffer.from(bindingSource).toString('base64')}#${encodeURIComponent(workerData.client)}`;
  const lsp = await import(bindingUrl);
  await lsp.default({ module_or_path: await wasmResponse.arrayBuffer() });
  const lines = workerData.document.split(/\r?\n/);
  const lastLine = lines.at(-1) ?? '';
  const serialized = lsp.inlay_hints(
    workerData.document,
    0,
    0,
    lines.length - 1,
    [...lastLine].length,
  );
  parentPort.postMessage({
    client: workerData.client,
    hints: JSON.parse(serialized),
  });
} catch (error) {
  parentPort.postMessage({
    error: error instanceof Error ? error.message : String(error),
  });
}
