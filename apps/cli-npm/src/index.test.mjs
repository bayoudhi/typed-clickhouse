import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "fs";
import { join } from "path";

const pkg = JSON.parse(
  readFileSync(join(import.meta.dirname, "..", "package.json"), "utf-8"),
);

test("is published under the @typed-clickhouse scope", () => {
  assert.equal(pkg.name, "@typed-clickhouse/cli");
});

test("exposes a single typed-clickhouse bin", () => {
  assert.deepEqual(pkg.bin, { "typed-clickhouse": "dist/index.js" });
});

// The platform packages are injected by release-cli.sh at publish time rather
// than committed here: they do not exist on the registry until a release
// publishes them, and a committed dependency on a nonexistent package makes
// `pnpm install --frozen-lockfile` unsatisfiable in CI.
const releaseScript = readFileSync(
  join(import.meta.dirname, "..", "scripts", "release-cli.sh"),
  "utf-8",
);

test("does not commit platform packages into the manifest", () => {
  assert.equal(pkg.optionalDependencies, undefined);
});

test("release-cli.sh declares exactly the three live platform packages", () => {
  const declared = [
    ...releaseScript.matchAll(/"(@typed-clickhouse\/cli-[a-z0-9-]+)"/g),
  ].map((m) => m[1]);
  assert.deepEqual(declared.sort(), [
    "@typed-clickhouse/cli-darwin-arm64",
    "@typed-clickhouse/cli-linux-arm64",
    "@typed-clickhouse/cli-linux-x64",
  ]);
});

test("does not reference the dead darwin-x64 package", () => {
  assert.ok(!releaseScript.includes("@typed-clickhouse/cli-darwin-x64"));
});

test("resolves the binary from the @typed-clickhouse scope", () => {
  const src = readFileSync(join(import.meta.dirname, "index.ts"), "utf-8");
  assert.ok(
    src.includes("@typed-clickhouse/cli-${os}-${arch}/bin/typed-clickhouse"),
  );
  assert.ok(!src.includes("@514labs/moose-cli-"));
});

test("keeps the template in the @typed-clickhouse scope", () => {
  const tmpl = readFileSync(
    join(import.meta.dirname, "..", "package.json.tmpl"),
    "utf-8",
  );
  assert.ok(tmpl.includes('"@typed-clickhouse/${node_pkg}"'));
  assert.ok(!tmpl.includes("@514labs"));
});
