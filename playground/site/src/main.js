import * as monaco from "monaco-editor";
import EditorWorker from "monaco-editor/editor/editor.worker.js?worker";
import initializeWasm, { ReplSession, run } from "../../orna-wasm/pkg/orna_wasm.js";
import "./style.css";

self.MonacoEnvironment = {
  getWorker() {
    return new EditorWorker();
  },
};

monaco.languages.register({ id: "orna" });
monaco.languages.setMonarchTokensProvider("orna", {
  tokenizer: {
    root: [
      [/\b(?:let|fn|pub|use|if|else|match|type|table)\b/, "keyword"],
      [/\b\d+(?:\.\d+)?\b/, "number"],
      [/"([^"\\]|\\.)*"/, "string"],
      [/\/\/.*$/, "comment"],
      [/[a-zA-Z_][\w.]*/, "identifier"],
    ],
  },
});

await initializeWasm();

document.querySelector("#app").innerHTML = `
  <main class="playground">
    <header class="topbar">
      <div class="brand"><span class="brand-mark">O</span><span>Orna Playground</span></div>
      <span class="runtime-status"><span class="status-dot"></span>WASM ready</span>
    </header>
    <section class="workspace" aria-label="Orna playground">
      <section class="editor-panel" aria-label="Source editor">
        <div class="panel-heading"><span>main.orna</span><button id="run-code" class="primary-button">Run <kbd>⌘ ↵</kbd></button></div>
        <div id="editor"></div>
      </section>
      <section class="output-panel" aria-label="Execution output">
        <div class="panel-heading"><span>Output</span><button id="clear-output" class="quiet-button">Clear</button></div>
        <div id="output" class="output" aria-live="polite"><p class="empty-state">Run your Orna source to see results here.</p></div>
        <form id="repl-form" class="repl-form">
          <label for="repl-input">REPL</label>
          <input id="repl-input" autocomplete="off" spellcheck="false" placeholder="Evaluate one line…" />
          <button class="quiet-button" type="submit">Evaluate</button>
        </form>
      </section>
    </section>
  </main>
`;

const editor = monaco.editor.create(document.querySelector("#editor"), {
  value: "let answer = 40;\nanswer + 2",
  language: "orna",
  theme: "vs-dark",
  automaticLayout: true,
  minimap: { enabled: false },
  scrollBeyondLastLine: false,
  fontSize: 14,
  tabSize: 2,
});
const output = document.querySelector("#output");
const repl = new ReplSession();

function addOutput(kind, text) {
  const row = document.createElement("div");
  row.className = `output-row ${kind}`;
  const marker = document.createElement("span");
  marker.className = "output-marker";
  marker.textContent = kind === "error" ? "!" : kind === "echo" ? "›" : "←";
  const content = document.createElement("pre");
  content.textContent = text;
  row.append(marker, content);
  if (output.querySelector(".empty-state")) output.replaceChildren();
  output.append(row);
}

document.querySelector("#run-code").addEventListener("click", () => {
  output.replaceChildren();
  const result = JSON.parse(run(editor.getValue()));
  for (const value of result.values) addOutput("value", value);
  for (const error of result.errors) addOutput("error", `${error.message} (line ${error.line}, col ${error.col})`);
  if (result.values.length === 0 && result.errors.length === 0) {
    output.innerHTML = '<p class="empty-state">Source evaluated without a value.</p>';
  }
});

document.querySelector("#clear-output").addEventListener("click", () => {
  output.innerHTML = '<p class="empty-state">Run your Orna source to see results here.</p>';
});

document.querySelector("#repl-form").addEventListener("submit", (event) => {
  event.preventDefault();
  const input = document.querySelector("#repl-input");
  if (!input.value.trim()) return;
  const result = JSON.parse(repl.evaluate(input.value));
  addOutput("echo", input.value);
  addOutput(result.kind, result.text);
  input.value = "";
});

editor.addCommand(monaco.KeyMod.CtrlCmd | monaco.KeyCode.Enter, () => {
  document.querySelector("#run-code").click();
});
