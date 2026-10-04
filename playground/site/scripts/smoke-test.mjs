import assert from "node:assert/strict";
import { createServer } from "node:http";
import { readFile, readdir, stat } from "node:fs/promises";
import { extname, join, resolve, sep } from "node:path";

const dist = resolve("dist");
const base = process.env.VITE_BASE_PATH ?? "./";
const mountPath = base.startsWith("/") ? base.replace(/\/$/, "") : "";
const contentTypes = {
  ".css": "text/css",
  ".html": "text/html",
  ".js": "text/javascript",
  ".json": "application/json",
  ".wasm": "application/wasm",
};

async function filesUnder(directory) {
  const files = [];
  for (const entry of await readdir(directory, { withFileTypes: true })) {
    const path = join(directory, entry.name);
    if (entry.isDirectory()) files.push(...await filesUnder(path));
    else files.push(path);
  }
  return files;
}

const index = await readFile(join(dist, "index.html"), "utf8");
assert.match(index, /Orna Playground/);
assert.match(index, /<script[^>]+type="module"/);

const artifacts = await filesUnder(dist);
assert.ok(artifacts.some((path) => path.endsWith(".js")), "bundled JavaScript exists");
assert.ok(artifacts.some((path) => path.endsWith(".css")), "bundled styles exist");
const wasmFiles = artifacts.filter((path) => path.endsWith(".wasm"));
assert.ok(wasmFiles.length > 0, "compiled WebAssembly is in the Pages artifact");
for (const path of wasmFiles) {
  const bytes = await readFile(path);
  assert.ok(bytes.byteLength > 0, `${path} is not empty`);
  await WebAssembly.compile(bytes);
}

const server = createServer(async (request, response) => {
  try {
    let pathname = decodeURIComponent(new URL(request.url, "http://localhost").pathname);
    if (mountPath && (pathname === mountPath || pathname.startsWith(`${mountPath}/`))) {
      pathname = pathname.slice(mountPath.length);
    }
    if (!pathname || pathname === "/") pathname = "/index.html";
    const file = resolve(dist, `.${pathname}`);
    assert.ok(file.startsWith(`${dist}${sep}`) || file === join(dist, "index.html"));
    const metadata = await stat(file);
    const body = await readFile(metadata.isDirectory() ? join(file, "index.html") : file);
    response.writeHead(200, { "content-type": contentTypes[extname(file)] ?? "application/octet-stream" });
    response.end(body);
  } catch {
    response.writeHead(404);
    response.end("not found");
  }
});

await new Promise((done) => server.listen(0, "127.0.0.1", done));
try {
  const address = server.address();
  const rootUrl = `http://127.0.0.1:${address.port}${mountPath}/`;
  const response = await fetch(rootUrl);
  assert.equal(response.status, 200, "the built site serves at its Pages base path");
  const servedIndex = await response.text();
  const modulePath = servedIndex.match(/<script[^>]+src="([^"]+)"/)?.[1];
  assert.ok(modulePath, "the served page references its app module");
  const moduleResponse = await fetch(new URL(modulePath, rootUrl));
  assert.equal(moduleResponse.status, 200, "the app module is served");
  assert.match(await moduleResponse.text(), /Orna Playground|monaco-editor/);
} finally {
  await new Promise((done, fail) => server.close((error) => error ? fail(error) : done()));
}

console.log(`PASS: served ${artifacts.length} built files; compiled ${wasmFiles.length} WebAssembly module(s).`);
