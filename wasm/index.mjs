import { initSync, parse as __wbg_parse } from "./pkg/cjs-module-lexer.js";
const wasmPath = "./pkg/cjs-module-lexer_bg.wasm";

let wasm;
if (globalThis.process || globalThis.Deno || globalThis.Bun) {
  const { readFileSync } = await import("node:fs");
  const url = new URL(wasmPath, import.meta.url);
  const wasmData = readFileSync(url.pathname);
  wasm = await WebAssembly.compile(wasmData);
} else {
  const url = new URL(wasmPath, import.meta.url);
  const pkgPrefix = "/@esm.sh/cjs-module-lexer@";
  if (url.pathname.startsWith(pkgPrefix)) {
    const version = url.pathname.slice(pkgPrefix.length).split("/", 1)[0];
    url.pathname = pkgPrefix + version + wasmPath.slice(1);
  }
  const res = await fetch(url);
  if (!res.ok) {
    throw new Error(`failed to fetch wasm: ${res.statusText}`);
  }
  wasm = await WebAssembly.compileStreaming(res);
}

initSync({ module: wasm });

/**
 * parse the given code and return the exports and reexports
 * @param {string} filename
 * @param {string} code
 * @param {{ nodeEnv?: 'development' | 'production', callMode?: boolean }} options
 * @returns {{ exports: string[], reexports: string[] }}
 */
export function parse(filename, code, options = {}) {
  return __wbg_parse(filename, code, options);
}
