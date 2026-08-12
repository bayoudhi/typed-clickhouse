#/usr/bin/env bash

set -eo pipefail

export node_version=$1
build_target=$2
# Reserved positional: kept to preserve the four-argument calling convention
# (node_version, build_target, build_os, build_name) even though build_os is
# currently unused. Do not remove or reorder without updating callers.
build_os=$3
build_name=$4

# name of the compiled binary, i.e. the crate name in
# apps/cli/Cargo.toml. src/index.ts resolves this same filename
# inside the installed platform package, so the two must stay in sync.
current_bin="typed-clickhouse"
# basename of the published platform package. The @typed-clickhouse scope is
# applied by package.json.tmpl, giving @typed-clickhouse/cli-<os>-<arch>.
pkg_base="cli"
# derive the OS and architecture from the build matrix name
# note: when split by a hyphen, first part is the OS and the second is the architecture
node_os=$(echo ${build_name} | cut -d '-' -f1)
export node_os
node_arch=$(echo ${build_name} | cut -d '-' -f2)
export node_arch

# set the package name
export node_pkg="${pkg_base}-${node_os}-${node_arch}"
# create the package directory
mkdir -p "${node_pkg}/bin"
# generate package.json from the template
envsubst < package.json.tmpl > "${node_pkg}/package.json"
# copy the binary into the package
ls "../../target/${build_target}/release/${current_bin}"
cp "../../target/${build_target}/release/${current_bin}" "${node_pkg}/bin"
# copy the generated third-party notices into the package. This binary
# statically links its entire Rust dependency tree, so the notices must
# ship in the same tarball; package.json.tmpl's "files" array only allows
# "bin" and "THIRD-PARTY-NOTICES.md" through, so a missing file here means
# a platform package publishes without them rather than silently omitting
# them -- fail loudly instead.
if [ ! -f "../../THIRD-PARTY-NOTICES.md" ]; then
    echo "../../THIRD-PARTY-NOTICES.md not found; run scripts/gen-third-party-notices.sh first" >&2
    exit 1
fi
cp "../../THIRD-PARTY-NOTICES.md" "${node_pkg}/"
cd "${node_pkg}"

# PACK_ONLY builds the tarball and stops short of the registry.
#
# The platform packages are built by a three-leg matrix, one per target. If each
# leg published its own package, a failure on any leg after the others had
# published would leave that version half-released on npm: published packages
# are immutable, so the version is burnt and the release has to move to the next
# number. That is not hypothetical -- a flaky protoc download failed exactly one
# leg during a dry run, and a vendored-OpenSSL or linker break would do the same
# for real. So every leg packs, and a single downstream job publishes the
# complete set only once all three have succeeded.
if [ "${PACK_ONLY}" = "true" ]; then
    pack_destination="${PACK_DESTINATION:-.}"
    # npm pack does not create --pack-destination, it just fails to open the
    # tarball inside it: "ENOENT: no such file or directory, open '.../foo.tgz'".
    mkdir -p "${pack_destination}"
    npm pack --pack-destination "${pack_destination}"
    exit 0
fi

# publish the package
# For CI builds (TAG_LATEST=false), publish with version-specific tag
# For release builds (TAG_LATEST=true), publish and update the 'latest' tag
if [ "${TAG_LATEST}" = "true" ]; then
    # Release build - publish and update 'latest' tag
    npm publish --access public
else
    # CI build - publish with dev tag (doesn't update 'latest')
    npm publish --access public --tag dev
fi