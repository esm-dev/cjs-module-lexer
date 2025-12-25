import { parse } from "./index.mjs";

const { exports, reexports } = parse("test.cjs", `
  module.exports = require("./lib");
  exports.a = "a";
  module.exports.b = "b";
  Object.defineProperty(exports, "c", { value: 1 });
  Object.defineProperty(module.exports, "__esModule", { value: true })
  const key = "foo"
  Object.defineProperty(exports, key, { value: "e" });
`);
if (exports.join(",") !== "a,b,c,__esModule,foo") {
  throw new Error("exports is expected to be a,b,c,__esModule,foo, but got " + exports.join(","));
}
if (reexports.join(",") !== "./lib") {
  throw new Error("reexports is expected to be ./lib, but got " + reexports.join(","));
}
console.log("✅ test passed");
