#!/usr/bin/env node
// Prove the generated VS Code extension's launch contract.
//
// `editors/vscode/extension.js` is generated, so the drift gate in
// `crates/orna-syntax-v1/tests/editor_artifacts.rs` proves its bytes are the
// emitter's bytes. That gate cannot see whether the client it renders still
// starts the server, reports a missing command without failing activation, or
// restarts on a setting change, so this script drives the generated file
// through those paths.
//
// The `vscode` API and the client library are stubbed: this script proves what
// the extension decides (the argv it builds, the failure it reports, the
// restart it performs), not what `vscode-languageclient` does with it. Pass
// `--real` after `npm install` in `editors/vscode` to also construct a client
// with the real library, which proves the generated launch options are the
// shape that version expects.
//
// Usage: node scripts/check_vscode_extension.js [--real]

"use strict";

const assert = require("assert");
const Module = require("module");
const path = require("path");

const EXTENSION = path.join(__dirname, "..", "editors", "vscode", "extension.js");
const EXTENSION_DIR = path.dirname(EXTENSION);
const SERVER_COMMAND_SETTING = "orna.lsp.serverCommand";
const SPAWN_FAILURE =
  "Launching server using command orna-lsp failed. Error: spawn orna-lsp ENOENT";

/** A stubbed `vscode` host whose setting value the script controls. */
function stubHost() {
  const host = {
    configured: undefined,
    errors: [],
    watchers: [],
    listeners: [],
    clients: [],
    startOutcome: () => Promise.resolve(),
  };
  host.api = {
    Disposable: class {
      constructor(dispose) {
        this.dispose = dispose;
      }
    },
    workspace: {
      getConfiguration() {
        return {
          get: (key) => (key === SERVER_COMMAND_SETTING ? host.configured : undefined),
        };
      },
      createFileSystemWatcher(glob) {
        host.watchers.push(glob);
        return { dispose() {} };
      },
      onDidChangeConfiguration(listener) {
        host.listeners.push(listener);
        return { dispose() {} };
      },
    },
    window: {
      showErrorMessage(message) {
        host.errors.push(message);
        return Promise.resolve();
      },
    },
  };
  host.clientLibrary = {
    LanguageClient: class {
      constructor(id, name, serverOptions, clientOptions) {
        Object.assign(this, { id, name, serverOptions, clientOptions, stops: 0 });
        host.clients.push(this);
      }
      start() {
        return host.startOutcome();
      }
      stop() {
        this.stops += 1;
        return Promise.resolve();
      }
    },
  };
  return host;
}

/** Load the generated extension against a stubbed host and client library. */
function loadExtension(host) {
  const load = Module._load;
  Module._load = function (request, ...rest) {
    if (request === "vscode") return host.api;
    if (request === "vscode-languageclient/node") return host.clientLibrary;
    return load.call(this, request, ...rest);
  };
  try {
    delete require.cache[require.resolve(EXTENSION)];
    return require(EXTENSION);
  } finally {
    Module._load = load;
  }
}

const tick = () => new Promise((resolve) => setImmediate(resolve));
const context = () => ({ subscriptions: [] });
const last = (items) => items[items.length - 1];

/** Construct a client with the real library against a stubbed host. */
function checkRealLibrary() {
  const disposable = () => ({ dispose() {} });
  const autoObject = (overrides = {}) =>
    new Proxy(overrides, {
      get(target, property) {
        if (!(property in target)) {
          const Inert = class {
            constructor(...args) {
              Object.assign(this, ...args.filter((a) => a && typeof a === "object"));
            }
          };
          target[property] = new Proxy(Inert, { apply: () => disposable() });
        }
        return target[property];
      },
    });
  const host = autoObject({
    version: "1.95.0",
    EventEmitter: class {
      constructor() {
        this.listeners = [];
      }
      get event() {
        return (listener) => {
          this.listeners.push(listener);
          return disposable();
        };
      }
      fire(value) {
        for (const listener of this.listeners) listener(value);
      }
      dispose() {
        this.listeners = [];
      }
    },
    workspace: autoObject({
      getConfiguration: () => ({ get: () => undefined }),
      createFileSystemWatcher: () => disposable(),
      workspaceFolders: undefined,
      textDocuments: [],
    }),
    window: autoObject({
      showErrorMessage: () => Promise.resolve(),
      visibleTextEditors: [],
      activeTextEditor: undefined,
    }),
    languages: autoObject({
      createDiagnosticCollection: () => ({ set() {}, clear() {}, dispose() {} }),
    }),
    commands: autoObject(),
  });
  const load = Module._load;
  Module._load = function (request, ...rest) {
    if (request === "vscode") return host;
    return load.call(this, request, ...rest);
  };
  try {
    const resolve = Module.createRequire(path.join(EXTENSION_DIR, "extension.js"));
    let library;
    let version;
    try {
      library = resolve("vscode-languageclient/node");
      version = resolve("vscode-languageclient/package.json").version;
    } catch (error) {
      if (error && error.code === "MODULE_NOT_FOUND") {
        console.log(
          "SKIP: vscode-languageclient is not installed in editors/vscode; run `npm install` there for the real-library probe",
        );
        return;
      }
      throw error;
    }
    assert.strictEqual(typeof library.LanguageClient, "function", "client library exports LanguageClient");
    const client = new library.LanguageClient(
      "orna",
      "Orna Language Server",
      { run: { command: "orna-lsp", args: [] }, debug: { command: "orna-lsp", args: [] } },
      {
        documentSelector: [{ scheme: "file", language: "orna" }],
        synchronize: { fileEvents: host.workspace.createFileSystemWatcher("**/*.orna") },
        connectionOptions: { maxRestartCount: 0 },
      },
    );
    assert.strictEqual(client.needsStart(), true, "a fresh client needs start");
    console.log(`PASS the installed vscode-languageclient ${version} accepts the generated launch options`);
  } finally {
    Module._load = load;
  }
}

async function main() {
  const real = process.argv.includes("--real");
  const host = stubHost();
  const extension = loadExtension(host);

  // 1. No setting: the server is `orna-lsp` with no arguments, stdio by default.
  extension.activate(context());
  await tick();
  assert.deepStrictEqual(
    last(host.clients).serverOptions.run,
    { command: "orna-lsp", args: [] },
    "the default command is orna-lsp with no arguments",
  );
  assert.deepStrictEqual(
    last(host.clients).serverOptions.debug,
    { command: "orna-lsp", args: [] },
    "the debug launch matches the run launch",
  );
  assert.deepStrictEqual(
    last(host.clients).clientOptions.documentSelector,
    [
      { scheme: "file", language: "orna" },
      { scheme: "untitled", language: "orna" },
    ],
    "the client attaches to Orna documents only",
  );
  assert.deepStrictEqual(host.watchers, ["**/*.orna"], "the client watches .orna files");
  console.log("PASS the extension starts orna-lsp with no arguments");

  // 2. An override is an argv list: executable first, arguments after.
  host.configured = ["/opt/orna/bin/orna-lsp", "--verbose"];
  await extension.deactivate();
  extension.activate(context());
  await tick();
  assert.deepStrictEqual(
    last(host.clients).serverOptions.run,
    { command: "/opt/orna/bin/orna-lsp", args: ["--verbose"] },
    "the override is split into executable and arguments",
  );
  console.log("PASS the override is passed through as an argv list");

  // 3. An empty override means the default.
  host.configured = [];
  await extension.deactivate();
  extension.activate(context());
  await tick();
  assert.deepStrictEqual(
    last(host.clients).serverOptions.run,
    { command: "orna-lsp", args: [] },
    "an empty override falls back to orna-lsp",
  );
  console.log("PASS an empty override means orna-lsp");

  // 4. A missing command reports once and does not fail activation.
  host.configured = undefined;
  host.startOutcome = () => Promise.reject(SPAWN_FAILURE);
  host.errors.length = 0;
  await extension.deactivate();
  const before = host.clients.length;
  extension.activate(context());
  await tick();
  assert.strictEqual(host.clients.length, before + 1, "activation creates one client");
  assert.strictEqual(host.errors.length, 1, "a missing server reports exactly once");
  assert.ok(host.errors[0].includes(SPAWN_FAILURE), "the report names the spawn failure");
  assert.ok(
    host.errors[0].includes(SERVER_COMMAND_SETTING),
    "the report names the setting to fix",
  );
  console.log("PASS a missing server reports once and activation still succeeds");

  // 5. Changing the setting restarts the client; other settings do not.
  host.startOutcome = () => Promise.resolve();
  await extension.deactivate();
  extension.activate(context());
  await tick();
  const previous = last(host.clients);
  host.configured = ["/usr/local/bin/orna-lsp"];
  last(host.listeners)({ affectsConfiguration: (key) => key === SERVER_COMMAND_SETTING });
  await tick();
  assert.strictEqual(previous.stops, 1, "the replaced client is stopped");
  assert.deepStrictEqual(
    last(host.clients).serverOptions.run,
    { command: "/usr/local/bin/orna-lsp", args: [] },
    "the restarted client uses the new command",
  );
  const settled = host.clients.length;
  last(host.listeners)({ affectsConfiguration: (key) => key !== SERVER_COMMAND_SETTING });
  await tick();
  assert.strictEqual(host.clients.length, settled, "unrelated settings do not restart the client");
  console.log("PASS changing the setting restarts the client and only that setting does");

  await extension.deactivate();
  if (real) {
    checkRealLibrary();
  } else {
    console.log("SKIP: real-library probe not requested (pass --real with editors/vscode/node_modules installed)");
  }
}

main().catch((error) => {
  console.error("FAILED:", error && error.message ? error.message : error);
  process.exitCode = 1;
});
