#!/usr/bin/env bash
# Static assertions on the release scripts. No publishing happens here.
set -euo pipefail
cd "$(dirname "$0")"

fail() { echo "FAIL: $1"; exit 1; }

grep -q '@typed-clickhouse/cli' release-cli.sh || fail "release-cli.sh does not filter on @typed-clickhouse/cli"
grep -q '@514labs' release-cli.sh && fail "release-cli.sh still references @514labs"
grep -q 'windows-2022' release-bin.sh && fail "release-bin.sh still has Windows branches"
grep -q 'TAG_LATEST' release-bin.sh || fail "release-bin.sh lost its TAG_LATEST handling"
grep -q 'set -eo pipefail' release-bin.sh || fail "release-bin.sh must fail fast"

# Argument-order contract: release-bin.sh binds its positionals as
# (node_version=$1, build_target=$2, build_os=$3, build_name=$4). Swapping
# build_target and build_os would not be caught by any other check here, so
# pin each variable to its expected positional index explicitly.
grep -q '^export node_version=\$1' release-bin.sh || fail "release-bin.sh must bind node_version to \$1"
grep -q '^build_target=\$2' release-bin.sh || fail "release-bin.sh must bind build_target to \$2"
grep -q '^build_os=\$3' release-bin.sh || fail "release-bin.sh must bind build_os to \$3"
grep -q '^build_name=\$4' release-bin.sh || fail "release-bin.sh must bind build_name to \$4"

# The committed manifest must not declare the platform packages. They do not
# exist on the registry until a release publishes them, so committing them
# makes `pnpm install --frozen-lockfile` unsatisfiable in CI. release-cli.sh
# injects them at publish time instead.
jq -e 'has("optionalDependencies")' ../package.json >/dev/null 2>&1 \
  && fail "package.json must not commit optionalDependencies; release-cli.sh injects them"

# Every platform package release-cli.sh injects must have a matching entry in
# the release build matrix, otherwise the wrapper would depend on a package
# that never gets published.
workflow=../../../.github/workflows/release.yaml
platform_packages=$(sed -n '/^platform_packages=(/,/^)/p' release-cli.sh | grep -o '@typed-clickhouse/cli-[a-z0-9-]*')
[ -n "$platform_packages" ] || fail "release-cli.sh declares no platform_packages"
for pkg in $platform_packages; do
  name="${pkg#@typed-clickhouse/cli-}"
  grep -q "NAME: ${name}," "$workflow" || fail "release.yaml build matrix has no entry for ${name}"
done

echo "PASS: release scripts are correctly scoped"
