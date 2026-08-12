import { readFileSync } from "fs";
import { resolve } from "path";
import { defineConfig, type Options } from "tsup";

const version = JSON.parse(
  readFileSync(resolve(__dirname, "package.json"), "utf8"),
).version as string;

const shared: Partial<Options> = {
  format: ["cjs"],
  outDir: "dist",
  splitting: false,
  clean: false,
  define: { __TCH_VERSION__: JSON.stringify(version) },
};

const libraryConfig: Options = {
  ...shared,
  entry: ["src/index.ts"],
  format: ["cjs", "esm"],
  // Plain `dts: true` only bundles types for entry-local declarations; a
  // re-export of a bare specifier like `export * from "@typed-clickhouse/lib"`
  // is left as-is by rollup-plugin-dts rather than inlined. Since
  // @typed-clickhouse/lib is `private: true` and never published, that literal
  // specifier is unresolvable in a real consumer's node_modules — the
  // published .d.ts would reference a package that doesn't exist for them.
  // `resolve` tells rollup-plugin-dts to walk into that package's own type
  // declarations and inline them, the dts equivalent of `noExternal` above.
  dts: true,
  sourcemap: true,
  clean: true,
  noExternal: ["@typed-clickhouse/lib"],
  external: [
    "@clickhouse/client",
    "@clickhouse/client-web",
    "csv-parse",
    "jose",
    "toml",
    "typia",
    "typescript",
    "fs",
    "path",
    "process",
    "node:stream",
  ],
};

const compilerPluginConfig: Options = {
  ...shared,
  entry: ["src/compilerPlugin.ts"],
  dts: false,
  sourcemap: false,
  noExternal: ["@typed-clickhouse/lib"],
  external: [
    "typescript",
    "ts-patch",
    "typia",
    "typia/lib/programmers/*",
    "typia/lib/factories/*",
    "typia/lib/transformers/*",
    "typia/lib/schemas/*",
    "typia/lib/tags",
    "@clickhouse/client",
    "@clickhouse/client-web",
    "csv-parse",
    "jose",
    "toml",
    "fs",
    "path",
    "process",
    "node:fs",
    "node:stream",
  ],
};

const tchTspcConfig: Options = {
  ...shared,
  entry: ["src/tch-tspc.ts"],
  dts: false,
  sourcemap: false,
  noExternal: ["@typed-clickhouse/lib"],
  external: [
    "ts-patch",
    "typescript",
    "child_process",
    "fs",
    "path",
    "process",
  ],
  // No `banner` here: src/tch-tspc.ts already starts with its own
  // `#!/usr/bin/env node` line, which tsup/esbuild preserves automatically.
  // Adding a banner too duplicated it (two shebang lines), which Node
  // rejects as a syntax error on the second line.
};

const tchRunnerConfig: Options = {
  ...shared,
  entry: ["src/tch-runner.ts"],
  dts: false,
  sourcemap: false,
  // commander is bundled: consumers may hoist an incompatible major
  // (e.g. commander@2 via CDK), and v13's .argument() is required.
  noExternal: ["@typed-clickhouse/lib", "commander"],
  external: [
    "@clickhouse/client",
    "@clickhouse/client-web",
    "csv-parse",
    "jose",
    "toml",
    "ts-node",
    "tsconfig-paths",
    "ts-patch",
    "typescript",
    "async_hooks",
    "buffer",
    "child_process",
    "cluster",
    "crypto",
    "fs",
    "http",
    "os",
    "path",
    "perf_hooks",
    "process",
    "stream",
    "util",
  ],
  // No `banner` here either — see the comment in tchTspcConfig above;
  // src/tch-runner.ts also carries its own shebang line already.
};

export default defineConfig([
  libraryConfig,
  compilerPluginConfig,
  tchTspcConfig,
  tchRunnerConfig,
]);
