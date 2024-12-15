import { initSync, parse as __wbg_parse } from "./pkg/cjs-module-lexer.js";

let wasm;
const wasmPath = "./pkg/cjs-module-lexer_bg.wasm";
const wasmUrl = new URL(wasmPath, import.meta.url);

if (globalThis.process || globalThis.Deno || globalThis.Bun) {
  const { readFileSync } = await import("node:fs");
  const wasmData = readFileSync(wasmUrl.pathname);
  wasm = await WebAssembly.compile(wasmData);
} else {
  const pkgPrefix = "/@esm.sh/cjs-module-lexer@";
  if (wasmUrl.pathname.startsWith(pkgPrefix)) {
    // fix the wasm url for esm.sh usage
    const version = wasmUrl.pathname.slice(pkgPrefix.length).split("/", 1)[0];
    wasmUrl.pathname = pkgPrefix + version + wasmPath.slice(1);
  }
  const res = await fetch(wasmUrl);
  if (!res.ok) {
    throw new Error(`failed to fetch wasm: ${res.statusText}`);
  }
  wasm = await WebAssembly.compileStreaming(res);
}

initSync({ module: wasm });

/**
 * parse the given cjs module and return the name exports and reexports
 * @param {string} filename
 * @param {string} code
 * @param {{ nodeEnv?: 'development' | 'production', callMode?: boolean }} options
 * @returns {{ exports: string[], reexports: string[] }}
 */
export function parse(filename, code, options = {}) {
  return __wbg_parse(filename, code, options);
}
