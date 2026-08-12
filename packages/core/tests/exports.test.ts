// NOTE: plain `require` on purpose, not `import`. Node >=22's native
// TypeScript support runs `.ts` mocha specs through a dynamic `import()`
// first; with no top-level `import`/`export` syntax in this file Node's
// module-format sniffing resolves it as CommonJS (so `__dirname` etc. are
// defined), and the same file still works via ts-node's CJS require hook
// on older Node runtimes.
//
// The body is wrapped in an IIFE because that same absence of top-level
// import/export makes TypeScript treat the file as a global script rather
// than a module. Every spec here would otherwise share one scope with
// packaging.test.ts, which declares the same `assert`/`join`/`readFileSync`
// names -- "Cannot redeclare block-scoped variable". mocha registers suites
// at load time, so running inside an IIFE changes nothing about discovery.
(() => {
  const { strict: assert } = require("assert");
  const { existsSync, readFileSync } = require("fs");
  const { join } = require("path");

  const dist = join(__dirname, "..", "dist");

  /**
   * The public API of this package is what downstream projects import. This
   * fork deliberately narrows that surface to ClickHouse-only tooling --
   * streaming, workflow, and HTTP-API exports are gone -- so golden/exports.json
   * is a snapshot of the current, intentionally-narrowed surface (generated
   * from the local build), not a copy of the published 0.7.13 artifact.
   * Regenerate it whenever the surface intentionally changes; never hand-edit
   * it.
   */
  describe("export surface", () => {
    it("builds both module formats and their types", () => {
      for (const f of ["index.js", "index.mjs", "index.d.ts", "index.d.mts"]) {
        assert.ok(existsSync(join(dist, f)), `missing dist/${f}`);
      }
    });

    it("loads under CJS without a native-module crash", () => {
      const mod = require(join(dist, "index.js"));
      assert.ok(Object.keys(mod).length > 0);
    });

    it("exports exactly the symbols the published artifact did", () => {
      const expected: string[] = JSON.parse(
        readFileSync(join(__dirname, "golden", "exports.json"), "utf-8"),
      );
      const actual: string[] = Object.keys(
        require(join(dist, "index.js")),
      ).sort();

      const missing: string[] = [];
      for (const name of expected) {
        if (!actual.includes(name)) missing.push(name);
      }
      const added: string[] = [];
      for (const name of actual) {
        if (!expected.includes(name)) added.push(name);
      }

      assert.deepEqual(missing, [], `missing exports: ${missing.join(", ")}`);
      assert.deepEqual(
        added,
        [],
        `unexpected new exports: ${added.join(", ")}`,
      );
    });

    it("exports the serverless-specific ClickHouse configuration hook", () => {
      const mod = require(join(dist, "index.js"));
      assert.equal(typeof mod.configureClickHouse, "function");
    });

    it("exports the core SDK classes", () => {
      const mod = require(join(dist, "index.js"));
      for (const name of ["OlapTable", "MaterializedView", "View"]) {
        assert.ok(mod[name], `missing export: ${name}`);
      }
    });

    it("ships both CLI binaries", () => {
      for (const f of ["tch-tspc.js", "tch-runner.js"]) {
        assert.ok(existsSync(join(dist, f)), `missing dist/${f}`);
      }
    });
  });
})();
