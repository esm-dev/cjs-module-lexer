# cjs-module-lexer

A lexer for detecting the `module.exports` of a CJS module, written in Rust.

## Usage

cjs-module-lexer currently only supports Node.js environment. You can install it via npm CLI:

```bash
npm i @esm.sh/cjs-module-lexer
```

cjs-module-lexer provides a `parse` function that detects the `module.exports` of a commonjs module. The function returns an object with two properties: `exports` and `reexports`. The `exports` property is an array of the exported names, and the `reexports` property is an array of the reexported modules.

```js
const { parse } = require("@esm.sh/cjs-module-lexer");

// named exports by assignment
// exports: ["a", "b", "c", "__esModule", "foo"]
const { exports } = parse("index.cjs", `
  exports.a = "a";
  module.exports.b = "b";
  Object.defineProperty(exports, "c", { value: 1 });
  Object.defineProperty(module.exports, "__esModule", { value: true })
  const key = "foo"
  Object.defineProperty(exports, key, { value: "e" });
`);

// reexports
// reexports: ["./lib"]
const { reexports } = parse("index.cjs", `
  module.exports = require("./lib");
`);

// object exports(spread syntax supported)
// exports: ["foo", "baz"]
// reexports: ["./lib"]
const { exports, reexports } = parse("index.cjs", `
  const foo = "bar"
  const obj = { baz: 123 }
  module.exports = { foo, ...obj, ...require("./lib") };
`);

// if expression
// exports: ["foo", "cjs"]
const { exports } = parse("index.cjs", `
  module.exports.a = "a";
  if (true) {
    exports.foo = "bar";
  }
  const mtype = "cjs";
  if (mtype === "cjs") {
    exports.cjs = true;
  } else {
    exports.esm = true;
  }
  if (false) {
    exports.unreachable = true;
  }
`);

// condition exports by checking if `process.env.NODE_ENV` equals to `nodeEnv` option
// reexports: ["./index.development.js"]
const { reexports } = parse("index.cjs", `
  if (process.env.NODE_ENV === "development") {
    module.exports = require("./index.development.js")
  } else {
    module.exports = require("./index.production.js")
  }
`, { nodeEnv: "development" });

// block&IIFE
// exports: ["foo", "baz", "__esModule"]
const { exports } = parse("index.cjs", `
  {
    exports.foo = 'bar'
  }
  (function () {
    exports.baz = 'qux'
    if (true) {
      return
    }
    exports.unreachable = true
  })();
  exports.__esModule = true
`);

// UMD format
// exports: ["foo"]
const { exports } = parse("index.cjs", `
  (function (global, factory) {
    typeof exports === "object" && typeof module !== "undefined" ? factory(exports) :
    typeof define === "function" && define.amd ? define(["exports"], factory) :
    (factory((global.MMDParser = global.MMDParser || {})));
  }(this, function (exports) {
    exports.foo = "bar";
  }))
`);

// exports by calling a function
// exports: ["foo"]
const { exports } = parse("index.cjs", `
  function module() {
    return { foo: "bar" }
  }
  module.exports = module()
`);

// annotated export names for ESM import
// exports: ["foo", "bar"]
const { exports } = parse("lib.cjs", `
0 && (module.exports = {
 foo,
 bar,
})
`);

// call reexports
// reexports: ["./lib()"]
const { reexports } = parse("index.cjs", `
  module.exports = require("./lib")()
`);
// apply call reexports
// exports: ["foo"]
const { exports } = parse("lib.cjs", `
  module.exports = function() {
    return { foo: "bar" }
  }
`, { callMode: true });
```

The `parse` function has the following types definition:

```ts
export function parse(
  specifier: string,
  code: string,
  options? {
    nodeEnv?: 'development' | 'production',
    callMode?: boolean,
  }
): {
  exports: string[],
  reexports: string[],
};
```

## Development Setup

You will need [rust](https://www.rust-lang.org/tools/install) 1.56+ and [wasm-pack](https://rustwasm.github.io/wasm-pack/installer/).

## Build

```bash
wasm-pack build --target nodejs
```

## Run tests

```bash
cargo test --all
```
