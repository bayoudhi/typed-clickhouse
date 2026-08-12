import { register } from "ts-node";

register({
  require: ["tsconfig-paths/register"],
  esm: true,
  experimentalTsImportSpecifiers: true,
  compiler: "ts-patch/compiler",
  compilerOptions: {
    paths: { "@typed-clickhouse/lib": ["./src/"] },
    plugins: [
      {
        transform: `./dist/compilerPlugin.js`,
        transformProgram: false,
      },
      {
        transform: "typia/lib/transform",
      },
    ],
    experimentalDecorators: true,
  },
});
