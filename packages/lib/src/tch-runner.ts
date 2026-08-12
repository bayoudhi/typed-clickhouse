#!/usr/bin/env node

// This file is used to run the proper runners for tch based on the
// arguments passed to the file.
// It loads pre-compiled JavaScript - no ts-node required.

import { dumpInternalRegistry } from "./dmv2/internal";
import { runExportSerializer } from "./moduleExportSerializer";

import { Command } from "commander";

// Injected at build time by tsup `define`. Must equal the CLI version —
// apps/cli/src/framework/typescript/bin.rs:53 compares them with
// strict equality.
declare const __TCH_VERSION__: string;
const packageJson = { version: __TCH_VERSION__ };

const program = new Command();

program
  .name("tch-runner")
  .description("the library runner for various operations")
  .version(packageJson.version);

program
  .command("print-version")
  .description("Print the installed the library version")
  .action(() => {
    process.stdout.write(packageJson.version);
  });

program
  .command("dmv2-serializer")
  .description("Load DMv2 index")
  .action(async () => {
    await dumpInternalRegistry();
  });

program
  .command("export-serializer")
  .description("Run export serializer")
  .argument("<target-model>", "Target model to serialize")
  .action(async (targetModel) => {
    await runExportSerializer(targetModel);
  });

program.parse();
