#!/usr/bin/env node
// Re-exports the workspace library's tspc entry point. All patches
// that were previously applied to upstream's compiled output now live in
// packages/lib/src/tch-tspc.ts.
import "@typed-clickhouse/lib/dist/tch-tspc";
