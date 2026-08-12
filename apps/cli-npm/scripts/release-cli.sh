#/usr/bin/env bash

set -eo pipefail

# This script should be called from the root of the repository

version=$1

# Platform packages published by release-bin.sh. Must stay in sync with the
# build matrix in .github/workflows/release.yaml and with the wait loop in the
# publish-wrapper job.
#
# These are injected here rather than committed into package.json: the checked-in
# manifest has to stay installable under `pnpm install --frozen-lockfile` in CI,
# and pnpm cannot lock a dependency that does not exist on the registry yet.
platform_packages=(
  "@typed-clickhouse/cli-linux-x64"
  "@typed-clickhouse/cli-linux-arm64"
  "@typed-clickhouse/cli-darwin-arm64"
)

cd ./apps/cli-npm
npm version $version --no-git-tag-version

# pin every optional platform dependency to the BUILD version
for dep in "${platform_packages[@]}"; do
  jq \
    --arg DEP "$dep" \
    --arg VERSION "$version" \
    '.["optionalDependencies"][$DEP] = $VERSION' package.json > package.json.tmp \
    && mv package.json.tmp package.json
done
cd ../..

# # This is run twice since the change the value of the dependencies in the previous step
pnpm install --filter "@typed-clickhouse/cli" --no-frozen-lockfile # requires optional dependencies to be present in the registry
pnpm build --filter @typed-clickhouse/cli

cd apps/cli-npm
# For CI builds (TAG_LATEST=false), publish with version-specific tag
# For release builds (TAG_LATEST=true), publish and update the 'latest' tag
if [ "${TAG_LATEST}" = "true" ]; then
    # Release build - publish and update 'latest' tag
    pnpm publish --access public --no-git-checks
else
    # CI build - publish with dev tag (doesn't update 'latest')
    pnpm publish --access public --no-git-checks --tag dev
fi