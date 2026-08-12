#!/usr/bin/env bash
# Captures the schema JSON emitted by the CURRENTLY PUBLISHED library.
# Run this BEFORE migrating to a source build. Run it once.
#
# Produces golden/baseline.json - the serialized model registry the Rust CLI
# consumes - which golden.test.sh asserts a source build still reproduces.
#
# NOTE: fixtures/ now import this fork's current package name
# (@typed-clickhouse/core). The historical artifact installed below is still
# published under its old name, so re-running this script requires mapping the
# specifier in fixtures/app/index.ts and fixtures/tsconfig.json back to
# @bayoudhi/moose-lib-serverless first. The baseline is already captured and
# must not be regenerated; this script is kept only as the record of how.
#
# golden/exports.json is NOT produced here. This fork intentionally narrows
# the export surface (streaming/workflow/HTTP-API exports removed), so there
# is no "published baseline" for exports anymore -- it is a snapshot of the
# current local build. Regenerate it with:
#   pnpm --filter @typed-clickhouse/core build
#   node -e 'const m=require("./dist/index.js");require("fs").writeFileSync("tests/golden/exports.json",JSON.stringify(Object.keys(m).sort(),null,2)+"\n")'
# from packages/core, then review the diff before committing.
set -euo pipefail
cd "$(dirname "$0")"
TESTS_DIR=$PWD

BASELINE_VERSION=${BASELINE_VERSION:-0.7.13}

WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT

# moose-runner's dmv2-serializer loads ./dist/app/index.js, so the fixture has
# to be laid out as a Moose app rather than a loose module.
cp -R fixtures/app fixtures/tsconfig.json "$WORK/"
cd "$WORK"

npm init -y >/dev/null
npm install --silent \
  "@bayoudhi/moose-lib-serverless@${BASELINE_VERSION}" \
  typia@^9.6.1 \
  ts-patch@^3.3.0 \
  typescript@~5.9.2

npx moose-tspc

# dumpMooseInternal writes the serialized model registry; this is the artifact
# the Rust CLI consumes and therefore the thing that must not drift.
npx moose-runner dmv2-serializer > raw.json 2>runner.log || {
  echo "moose-runner failed; stderr follows:" >&2
  cat runner.log >&2
  exit 1
}

# The runner may interleave log lines with the JSON payload. Extract the JSON
# document only, and record the method here because golden.test.sh must extract
# it identically or the comparison is meaningless.
python3 - "$TESTS_DIR/golden/baseline.json" <<'PY'
import json, sys

raw = open("raw.json", encoding="utf-8").read()
try:
    doc = json.loads(raw)
except json.JSONDecodeError:
    # Fall back to the first balanced JSON value in the stream.
    start = min((i for i in (raw.find("{"), raw.find("[")) if i != -1), default=-1)
    if start == -1:
        sys.exit("no JSON found in moose-runner output")
    doc, _ = json.JSONDecoder().raw_decode(raw[start:])

with open(sys.argv[1], "w", encoding="utf-8") as fh:
    json.dump(doc, fh, indent=2, sort_keys=True)
    fh.write("\n")
PY

echo "Baseline captured from @bayoudhi/moose-lib-serverless@${BASELINE_VERSION}"
echo "  schema: $(wc -c < "$TESTS_DIR/golden/baseline.json" | tr -d ' ') bytes"
echo "(golden/exports.json is not touched by this script -- see the header comment)"
