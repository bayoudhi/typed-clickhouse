import { defineConfig } from "tsup";

export default defineConfig({
  entry: [
    "src/index.ts",
    "src/browserCompatible.ts",
    "src/serverless.ts",
    "src/dataModels/toDataModels.ts",
    "src/compilerPlugin.ts",
    "src/tch-tspc.ts",
    "src/tch-runner.ts",
    "src/dmv2/index.ts",
    "src/testing/index.ts",
  ],
  format: ["cjs", "esm"],
  dts: true, // Generate declaration file (.d.ts)
  outDir: "dist",
  splitting: false,
  sourcemap: true,
  clean: true,
});
