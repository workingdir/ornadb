// Tokenize a sample with the generated TextMate grammar through the VS Code
// engine (vscode-textmate with vscode-oniguruma) and print one
// "LINE START END CLASS" row per token, with zero-based UTF-8 byte columns.
//
//   ORNA_EDITOR_TOOLS=<dir with node_modules> node textmate.mjs GRAMMAR SAMPLE

import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { join } from "node:path";

const [grammarPath, samplePath] = process.argv.slice(2);
const tools = process.env.ORNA_EDITOR_TOOLS;
if (!tools || !grammarPath || !samplePath) {
  console.error("usage: ORNA_EDITOR_TOOLS=<dir> node textmate.mjs GRAMMAR SAMPLE");
  process.exit(2);
}
const require = createRequire(join(tools, "package.json"));
const textmate = require("vscode-textmate");
const oniguruma = require("vscode-oniguruma");

const wasm = readFileSync(require.resolve("vscode-oniguruma/release/onig.wasm"));
await oniguruma.loadWASM(wasm.buffer.slice(wasm.byteOffset, wasm.byteOffset + wasm.byteLength));
const registry = new textmate.Registry({
  onigLib: Promise.resolve({
    createOnigScanner: (patterns) => new oniguruma.OnigScanner(patterns),
    createOnigString: (text) => new oniguruma.OnigString(text),
  }),
  loadGrammar: async (scopeName) =>
    scopeName === "source.orna"
      ? textmate.parseRawGrammar(readFileSync(grammarPath, "utf8"), grammarPath)
      : null,
});
const grammar = await registry.loadGrammar("source.orna");

// Innermost scope that names a lexical class decides the token's class.
const classes = [
  ["comment.", "comment"],
  ["constant.character.escape.", "string"],
  ["string.", "string"],
  ["keyword.control.", "keyword"],
  ["keyword.operator.", "operator"],
  ["variable.", "identifier"],
  ["constant.numeric.", "number"],
  ["punctuation.", "punctuation"],
];
function classify(scopes) {
  if (scopes.some((scope) => scope.startsWith("comment."))) return "comment";
  for (const scope of [...scopes].reverse()) {
    const found = classes.find(([prefix]) => scope.startsWith(prefix));
    if (found) return found[1];
  }
  return "none";
}

const lines = readFileSync(samplePath, "utf8").split("\n");
if (lines.at(-1) === "") lines.pop();
let state = textmate.INITIAL;
const rows = [];
lines.forEach((line, index) => {
  const result = grammar.tokenizeLine(line, state);
  for (const token of result.tokens) {
    const start = Buffer.byteLength(line.slice(0, token.startIndex), "utf8");
    const end = Buffer.byteLength(line.slice(0, Math.min(token.endIndex, line.length)), "utf8");
    if (end > start) rows.push(`${index} ${start} ${end} ${classify(token.scopes)}`);
  }
  state = result.ruleStack;
});
console.log(rows.join("\n"));
