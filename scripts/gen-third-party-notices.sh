#!/usr/bin/env bash
# Generates THIRD-PARTY-NOTICES.md at the repo root, covering:
#
#  1. Every crate statically linked into the `typed-clickhouse` Rust binary
#     (via cargo-about + about.toml/about.hbs), including the license text
#     for each license family and, for any Apache-2.0 crate that ships one,
#     its upstream NOTICE file (Apache-2.0 sec. 4(d) requires this to travel
#     with the notice, and cargo-about's own templates do not collect it).
#  2. The third-party npm dependencies tsup actually bundles into
#     packages/core/dist/ (checked via noExternal in tsup.config.ts, not the
#     full devDependency list -- most deps are `external` and stay as real
#     npm dependencies the consumer installs, not code we redistribute).
#
# Not committed: this must be regenerated every release so it always reflects
# the dependency tree that shipped, not a stale snapshot. See release.yaml's
# `third-party-notices` job.
#
# Requires: cargo-about (`cargo install cargo-about --locked --features cli`)
# and `pnpm install` already run (so commander's LICENSE is on disk).
set -euo pipefail
cd "$(dirname "$0")/.."

command -v cargo-about >/dev/null 2>&1 || {
  echo "cargo-about not found; run: cargo install cargo-about --locked --features cli" >&2
  exit 1
}

OUT=THIRD-PARTY-NOTICES.md

echo "Generating Rust dependency notices with cargo-about..." >&2
( cd apps/cli && cargo about generate ../../about.hbs -c ../../about.toml -o "../../${OUT}" )

# --- Apache-2.0 upstream NOTICE files -------------------------------------
# cargo-about extracts license text but not NOTICE files, and Apache-2.0
# conditions redistribution on including any NOTICE file the licensor
# shipped. None of the crates in this tree currently ship one (checked by
# hand against the resolved Cargo.lock), but this scans on every run so a
# future dependency bump that adds one does not silently ship without it.
echo "Scanning for upstream NOTICE files..." >&2
notices_json_file=$(mktemp)
trap 'rm -f "$notices_json_file"' EXIT
( cd apps/cli && cargo about generate --format json -c ../../about.toml ) > "$notices_json_file"
notice_section=$(NOTICES_JSON_FILE="$notices_json_file" python3 - <<'PY'
import json, os, sys

with open(os.environ["NOTICES_JSON_FILE"], encoding="utf-8") as f:
    data = json.load(f)
seen = set()
found = []
for crate_entry in data["crates"]:
    pkg = crate_entry["package"]
    key = (pkg["name"], pkg["version"])
    if key in seen:
        continue
    seen.add(key)
    manifest_path = pkg.get("manifest_path")
    if not manifest_path:
        continue
    crate_dir = os.path.dirname(manifest_path)
    try:
        entries = os.listdir(crate_dir)
    except OSError:
        continue
    for fname in entries:
        if fname.upper().startswith("NOTICE"):
            with open(os.path.join(crate_dir, fname), encoding="utf-8", errors="replace") as f:
                text = f.read()
            found.append((pkg["name"], pkg["version"], fname, text))

if found:
    print("\n## Upstream NOTICE files\n")
    print(
        "The following crates ship an upstream NOTICE file; Apache-2.0 "
        "requires it to travel with the distributed binary.\n"
    )
    for name, version, fname, text in found:
        print(f"### {name} {version} ({fname})\n")
        print("```")
        print(text.rstrip("\n"))
        print("```\n")
PY
)
if [ -n "$notice_section" ]; then
  printf '%s\n' "$notice_section" >> "$OUT"
fi

# --- npm dependencies bundled into packages/core/dist ----------------------
# packages/core/tsup.config.ts's tchRunnerConfig is the only build target
# with a third-party package in `noExternal` (commander -- bundled because
# consumers may hoist an incompatible major elsewhere). Everything else in
# that config's `noExternal` is our own workspace package
# (@typed-clickhouse/lib), not third-party code, so it needs no notice.
echo "Resolving bundled npm dependency licenses..." >&2
{
  echo
  echo "## npm dependencies bundled into @typed-clickhouse/core"
  echo
  echo "tsup inlines the following third-party package into"
  echo '`dist/tch-runner.js` (see `noExternal` in'
  echo '`packages/core/tsup.config.ts`) rather than leaving it an installable'
  echo "dependency, so its license text must travel with that file too."
  echo
} >> "$OUT"

# Resolve via packages/core, the package that actually declares the
# `commander` dependency (see packages/core/package.json), rather than
# guessing at pnpm's node_modules/.pnpm layout directly.
commander_dir=$(cd packages/core && node -e '
  const path = require("path");
  const fs = require("fs");
  const entry = require.resolve("commander");
  let dir = path.dirname(entry);
  while (!fs.existsSync(path.join(dir, "package.json"))) {
    const parent = path.dirname(dir);
    if (parent === dir) throw new Error("commander package.json not found");
    dir = parent;
  }
  const pkg = require(path.join(dir, "package.json"));
  if (pkg.name !== "commander") throw new Error("resolved wrong package: " + pkg.name);
  console.log(dir);
')

commander_version=$(node -e "console.log(require('${commander_dir}/package.json').version)")
commander_license_file=""
for cand in LICENSE LICENSE.md LICENSE.txt; do
  if [ -f "${commander_dir}/${cand}" ]; then
    commander_license_file="${commander_dir}/${cand}"
    break
  fi
done
if [ -z "$commander_license_file" ]; then
  echo "commander's LICENSE file could not be found at ${commander_dir}" >&2
  exit 1
fi

{
  echo "### commander ${commander_version}"
  echo
  echo '```'
  cat "$commander_license_file"
  echo '```'
} >> "$OUT"

echo "Wrote ${OUT}" >&2
wc -l "$OUT" >&2
