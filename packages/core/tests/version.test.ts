// NOTE: plain `require` on purpose, not `import`. Node >=22's native
// TypeScript support runs `.ts` mocha specs through a dynamic `import()`
// first; with no top-level `import`/`export` syntax in this file Node's
// module-format sniffing resolves it as CommonJS (so `__dirname` etc. are
// defined), and the same file still works via ts-node's CJS require hook
// on older Node runtimes.
//
// The body is wrapped in an IIFE because that same absence of top-level
// import/export makes TypeScript treat the file as a global script rather
// than a module, sharing one scope with the other specs here. See the fuller
// note in exports.test.ts.
(() => {
  const { strict: assert } = require("assert");
  const { execFileSync } = require("child_process");
  const { readFileSync } = require("fs");
  const { join } = require("path");

  const root = join(__dirname, "..");
  const pkg = JSON.parse(readFileSync(join(root, "package.json"), "utf-8"));

  /**
   * apps/cli/src/framework/typescript/bin.rs runs
   * `tch-runner print-version` and compares it to the CLI's own version with
   * strict equality. The version is injected into the bundle at build time as
   * __TCH_VERSION__; this asserts that injection actually happened and
   * carries the package's version rather than a stale or empty constant.
   *
   * The CLI side of the equality is NOT asserted here. Cargo.toml is pinned at
   * the "0.0.1" sentinel that bin.rs keys on to skip the check for dev builds,
   * so comparing against it would either fail or force a bump that turns
   * strict checking on for every local build. Lockstep is enforced where it
   * actually happens instead -- both publish jobs consume one tag-derived
   * version, asserted in .github/workflows/release.test.sh.
   */
  describe("version alignment", () => {
    it("reports the package version from print-version", () => {
      const out = execFileSync(
        process.execPath,
        [join(root, "dist", "tch-runner.js"), "print-version"],
        { encoding: "utf-8" },
      ).trim();
      assert.equal(out, pkg.version);
    });

    it("does not ship an uninjected version placeholder", () => {
      const bundle = readFileSync(join(root, "dist", "tch-runner.js"), "utf-8");
      assert.ok(
        !bundle.includes("__TCH_VERSION__"),
        "__TCH_VERSION__ survived into the bundle -- tsup `define` did not replace it",
      );
    });
  });
})();
