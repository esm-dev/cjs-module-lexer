import { parse } from "./index.mjs";

console.log(parse("test.cjs", `
  exports.a = "a";
  module.exports.b = "b";
  Object.defineProperty(exports, "c", { value: 1 });
  Object.defineProperty(module.exports, "__esModule", { value: true })
  const key = "foo"
  Object.defineProperty(exports, key, { value: "e" });
`));
