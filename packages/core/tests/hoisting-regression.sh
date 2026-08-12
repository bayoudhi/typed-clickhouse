#!/usr/bin/env bash
# Reproduces the duplicate-typescript failure that the bundle-level
# import_typescript1..7 reassignment used to paper over.
#
# The plugin resolves `typescript` at module scope. If pnpm installs a second
# copy beside the consumer's, ts-patch loads one instance and the plugin
# compares against the other's SyntaxKind enums — every check fails, the
# transform silently no-ops, and the user sees "Supply the type param T".
#
# Strict hoisting (node-linker=isolated) is the condition that surfaces it.
set -euo pipefail
cd "$(dirname "$0")"

PKG=$(cd .. && pnpm pack --pack-destination /tmp | tail -1)
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT

# Same layout as capture-baseline.sh: tch-runner's dmv2-serializer loads
# ./dist/app/index.js, so the fixture is a the library app, not a loose module.
cp -R fixtures/app fixtures/tsconfig.json "$WORK/"
cd "$WORK"

cat > package.json <<'JSON'
{
  "name": "hoisting-fixture",
  "private": true,
  "dependencies": {
    "typia": "^9.6.1",
    "ts-patch": "^3.3.0",
    "typescript": "5.8.3"
  }
}
JSON

# 5.8.3 is deliberately OUTSIDE this package's `~5.9.2` range. A consumer on a
# satisfying version gets deduped to a single copy and the bug cannot appear;
# only a conflicting version forces pnpm to install a second TypeScript, which
# is the condition that produced "Supply the type param T".

cat > .npmrc <<'RC'
node-linker=isolated
shamefully-hoist=false
RC

pnpm add "$PKG" >/dev/null
pnpm install >/dev/null

echo "typescript copies installed:"
find node_modules/.pnpm -maxdepth 1 -name 'typescript@*' -type d | sed 's|.*/||'

set +e
OUT=$(npx tch-tspc 2>&1)
CODE=$?
set -e

echo "$OUT"

if echo "$OUT" | grep -q "Supply the type param"; then
  echo "FAIL: duplicate typescript instances — plugin did not transform"
  exit 1
fi
if [ $CODE -ne 0 ]; then
  echo "FAIL: tch-tspc exited $CODE"
  exit 1
fi

# Compiling without error is necessary but not sufficient: the failure mode
# this guards against is a SILENT no-op — the plugin runs, reports success and
# emits the constructor calls untouched.
#
# "Float64" is a ClickHouse column type. It appears nowhere in the fixture
# source (which says `count: number`); it exists in the output only because the
# plugin derived a schema from the type parameter and injected it. Untransformed
# output is ~1.4 KB, transformed is ~28 KB.
if ! grep -q 'Float64' dist/app/index.js; then
  echo "FAIL: compiled output carries no injected schema — transform no-opped"
  exit 1
fi

echo "PASS: compiled cleanly under strict hoisting, transform applied"
