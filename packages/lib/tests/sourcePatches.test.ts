import { strict as assert } from "assert";
import { readFileSync } from "fs";
import { join } from "path";
import { isLibraryFile } from "../src/compilerPluginHelper";
import { TCH_COMPILER_PLUGINS } from "../src/compiler-config";

const src = (f: string) =>
  readFileSync(join(__dirname, "..", "src", f), "utf-8");

describe("source-level patches", () => {
  it("recognizes the serverless package as a tch file", () => {
    const fake = {
      fileName: "/x/node_modules/@typed-clickhouse/core/dist/index.d.ts",
    };
    assert.equal(isLibraryFile(fake as any), true);
  });

  it("still recognizes workspace development paths", () => {
    assert.equal(
      isLibraryFile({ fileName: "/x/packages/lib/src/index.ts" } as any),
      true,
    );
  });

  it("no longer matches the retired @514labs path", () => {
    assert.equal(
      isLibraryFile({
        fileName: "/x/node_modules/@514labs/moose-lib/dist/index.d.ts",
      } as any),
      false,
    );
  });

  it("resolves the compiler plugin instead of hardcoding node_modules", () => {
    assert.ok(!TCH_COMPILER_PLUGINS[0].transform.includes("./node_modules/"));
    assert.ok(!TCH_COMPILER_PLUGINS[0].transform.includes("@514labs"));
  });

  it("invokes tspc through the running node binary, not npx", () => {
    const t = src("tch-tspc.ts");
    assert.ok(!t.includes('"npx"'), "npx must not be used");
    assert.ok(t.includes("process.execPath"));
    assert.ok(t.includes('require.resolve("ts-patch/bin/tspc.js")'));
  });

  it("does not force sourceMap flags that conflict with inlineSourceMap", () => {
    const t = src("tch-tspc.ts");
    assert.ok(!t.includes('"--sourceMap"'));
    assert.ok(!t.includes('"--inlineSources"'));
  });

  it("does not shell out to determine its own version", () => {
    const r = src("tch-runner.ts");
    assert.ok(!r.includes("tch --version"));
    assert.ok(!r.includes("execSync"));
  });
});
