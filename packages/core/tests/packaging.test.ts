// NOTE: plain `require` on purpose, not `import`. Node >=22's native
// TypeScript support runs `.ts` mocha specs through a dynamic `import()`
// first; with no top-level `import`/`export` syntax in this file Node's
// module-format sniffing resolves it as CommonJS (so `__dirname` etc. are
// defined), and the same file still works via ts-node's CJS require hook
// on older Node runtimes.
const { strict: assert } = require("assert");
const { readFileSync, existsSync } = require("fs");
const { join } = require("path");

const root = join(__dirname, "..");
const pkg = JSON.parse(readFileSync(join(root, "package.json"), "utf-8"));
const libPkg = JSON.parse(
  readFileSync(join(root, "..", "lib", "package.json"), "utf-8"),
);

describe("packaging", () => {
  it("has no @514labs dependency of any kind", () => {
    const all = {
      ...pkg.dependencies,
      ...pkg.devDependencies,
      ...pkg.peerDependencies,
    };
    const offenders = Object.keys(all).filter((d) => d.startsWith("@514labs/"));
    assert.deepEqual(offenders, []);
  });

  it("is published as @typed-clickhouse/core", () => {
    assert.equal(pkg.name, "@typed-clickhouse/core");
  });

  it("links the workspace library under the new scope", () => {
    assert.equal(pkg.devDependencies["@typed-clickhouse/lib"], "workspace:*");
  });

  it("ships the tch-prefixed bins", () => {
    assert.deepEqual(Object.keys(pkg.bin).sort(), ["tch-runner", "tch-tspc"]);
  });

  it("has no @bayoudhi dependency of any kind", () => {
    const all = {
      ...pkg.dependencies,
      ...pkg.devDependencies,
      ...pkg.peerDependencies,
    };
    assert.deepEqual(
      Object.keys(all).filter((d) => d.startsWith("@bayoudhi/")),
      [],
    );
  });

  it("keeps the workspace library private", () => {
    assert.equal(libPkg.name, "@typed-clickhouse/lib");
    assert.equal(libPkg.private, true);
  });

  // The workspace library is inlined into this bundle, so any native
  // dependency it declares would end up as a top-level require() that
  // crashes in Lambda. `stubNativeModules` used to paper over exactly that;
  // it was retired once these dependencies were dropped, and this assertion
  // is what stops them coming back.
  it("the inlined workspace library declares no native dependency", () => {
    const native = [
      "@514labs/kafka-javascript",
      "@kafkajs/confluent-schema-registry",
      "@temporalio/activity",
      "@temporalio/client",
      "@temporalio/common",
      "@temporalio/worker",
      "@temporalio/workflow",
      "redis",
    ];
    const declared = Object.keys({
      ...libPkg.dependencies,
      ...libPkg.peerDependencies,
    });
    assert.deepEqual(
      native.filter((d) => declared.includes(d)),
      [],
    );
  });

  it("starts at the 0.1.0 baseline", () => {
    assert.equal(pkg.version, "0.1.0");
  });
});

describe("build config", () => {
  const cfg = readFileSync(join(root, "tsup.config.ts"), "utf-8");

  it("no longer reads upstream compiled output", () => {
    assert.ok(!cfg.includes("node_modules/@514labs/moose-lib/dist"));
  });

  it("no longer hand-rolls type declarations", () => {
    assert.ok(!cfg.includes("copyUpstreamTypes"));
  });

  it("inlines the workspace library", () => {
    assert.ok(cfg.includes('noExternal: ["@typed-clickhouse/lib"]'));
  });

  it("no longer stubs native modules", () => {
    assert.ok(!cfg.includes("stubNativeModules"));
  });

  it("injects the version for tch-runner print-version", () => {
    assert.ok(cfg.includes("__TCH_VERSION__"));
  });
});

describe("built compiler plugin path resolution", () => {
  // packages/lib/src/compiler-config.ts resolves the compiler
  // plugin's tsconfig path via `require.resolve("@typed-clickhouse/
  // core/dist/compilerPlugin.js")`, falling back to the bare specifier
  // if resolution throws. the library's OWN test suite
  // (tests/sourcePatches.test.ts) only ever exercises the catch branch: that
  // package has no dependency on @typed-clickhouse/core, so require.resolve
  // always throws there, and asserting "not a hardcoded ./node_modules/
  // path" passes identically whether resolution succeeds or always fails.
  //
  // This package (@typed-clickhouse/core) IS the thing being resolved, and
  // this test file lives inside its own package tree, so Node's
  // self-reference resolution (package.json "name" + "exports") can actually
  // succeed here — exercising the success path the other test cannot reach,
  // and proving the built dist/compilerPlugin.js is what gets resolved
  // rather than silently falling back to the unresolvable bare specifier.
  it("resolves to a real file in dist/, not the bare-specifier fallback", () => {
    const bareSpecifier = "@typed-clickhouse/core/dist/compilerPlugin.js";
    const resolved = require.resolve(bareSpecifier);

    assert.notEqual(resolved, bareSpecifier);
    assert.ok(existsSync(resolved), `expected ${resolved} to exist`);
    assert.equal(resolved, join(root, "dist", "compilerPlugin.js"));
  });
});
