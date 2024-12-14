# Changelog

## 1.0.4

- native: Improve module resolver

## 1.0.3

- native: Improve arguments input
  ```bash
  npm i react
  NODE_ENV=development ./cjs-module-lexer react
  ```
- Change npm package to ESM

## 1.0.2

- Add native build
- Upgrade swc_esmascript 8.0.0

## 1.0.1

- Upgrade swc_esmascript 5.0.0

## 1.0.0

- Publish package under `@esm.sh` scope
- Upgrade swc_esmascript 4.0.0

## 0.11.0

- Check IIFE block under `&&` binary expression (close #1)
  ```js
  "production" !== process.env.NODE_ENV && (function () {
    module.exports = { foo: 'bar' }
  })()
  ```
- Support `Object.defineProperty((0, exports), "foo", { value: "bar" });` equivalent to `exports.foo = "bar";`

## 0.10.1

Moved the repository from https://github.com/esm-dev/esm.sh
