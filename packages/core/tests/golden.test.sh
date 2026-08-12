#!/usr/bin/env bash
# Asserts the source-built compiler plugin emits the same schema JSON as the
# published 0.7.13 artifact captured in golden/baseline.json.
#
# This is the phase gate. Everything else verifies that the build produces
# *something*; this verifies it produces the *same* thing.
set -euo pipefail
cd "$(dirname "$0")"
TESTS_DIR=$PWD

[ -f golden/baseline.json ] || { echo "FAIL: no baseline — run capture-baseline.sh first"; exit 1; }

PKG=$(cd .. && pnpm pack --pack-destination /tmp | tail -1)

# Copy to a unique filename. Package managers key their content-addressed store
# on the tarball path, so republishing the same version under the same name can
# silently install a stale copy and turn this gate into a no-op.
UNIQ="/tmp/golden-$(date +%s)-$(basename "$PKG")"
cp "$PKG" "$UNIQ"

WORK=$(mktemp -d)
trap 'rm -rf "$WORK" "$UNIQ"' EXIT

# Same layout as capture-baseline.sh: tch-runner's dmv2-serializer loads
# ./dist/app/index.js, so the fixture is a the library app, not a loose module.
cp -R fixtures/app fixtures/tsconfig.json "$WORK/"
cd "$WORK"

# npm, not pnpm, on purpose: a flat node_modules is the second resolution shape
# worth covering, complementing the strict-hoisting fixture in
# hoisting-regression.sh.
npm init -y >/dev/null
npm install --silent "$UNIQ" typia@^9.6.1 ts-patch@^3.3.0 typescript@~5.9.2

npx tch-tspc

npx tch-runner dmv2-serializer > raw.json 2>runner.log || {
  echo "FAIL: tch-runner failed; stderr follows:" >&2
  cat runner.log >&2
  exit 1
}

# Extraction matches capture-baseline.sh. Both sides are then re-serialized
# through the SAME writer before comparison, for two reasons:
#
#   1. golden/baseline.json is committed, and lint-staged runs `prettier --write`
#      on *.json. Prettier collapses short arrays onto one line, so the file in
#      git is no longer byte-identical to what json.dump produced. Comparing
#      text would report formatting as drift.
#   2. metadata.source.file records an absolute path containing the mktemp
#      directory, which differs on every run and between capture and replay.
#
# Normalizing both sides identically compares content, which is the thing that
# must not drift.
python3 - "$TESTS_DIR/golden/baseline.json" expected.json actual.json <<'PY'
import json, re, sys

baseline_path, expected_out, actual_out = sys.argv[1:4]

raw = open("raw.json", encoding="utf-8").read()
try:
    doc = json.loads(raw)
except json.JSONDecodeError:
    # Fall back to the first balanced JSON value in the stream.
    start = min((i for i in (raw.find("{"), raw.find("[")) if i != -1), default=-1)
    if start == -1:
        sys.exit("no JSON found in tch-runner output")
    doc, _ = json.JSONDecoder().raw_decode(raw[start:])

TEMP_PATH = re.compile(r"^/.*/(dist/app/[^\"]+)$")

def normalize(node):
    """Replace run-specific absolute paths with a stable token."""
    if isinstance(node, dict):
        return {k: normalize(v) for k, v in node.items()}
    if isinstance(node, list):
        return [normalize(v) for v in node]
    if isinstance(node, str):
        m = TEMP_PATH.match(node)
        if m:
            return "<WORKDIR>/" + m.group(1)
    return node

def write(obj, path):
    with open(path, "w", encoding="utf-8") as fh:
        json.dump(normalize(obj), fh, indent=2, sort_keys=True)
        fh.write("\n")

write(json.load(open(baseline_path, encoding="utf-8")), expected_out)
write(doc, actual_out)
PY

if diff -u expected.json actual.json; then
  echo "PASS: source build matches the published 0.7.13 baseline"
else
  echo "FAIL: schema output drifted from the baseline"
  exit 1
fi
