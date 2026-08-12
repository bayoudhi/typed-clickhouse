#!/usr/bin/env bash
# Static assertions on the release workflow.
set -euo pipefail
cd "$(dirname "$0")"

fail() { echo "FAIL: $1"; exit 1; }
W=release.yaml

[ -f "$W" ] || fail "release.yaml does not exist"
[ -f release-cli.yaml ] && fail "old release-cli.yaml still present"

grep -q 'ubuntu-22-8-core' "$W" && fail "still targets an unavailable large runner"
grep -q 'macos-14-large'   "$W" && fail "still targets an unavailable large runner"
grep -q 'x86_64-apple-darwin' "$W" && fail "Intel macOS is a non-goal"
grep -q 'windows' "$W" && fail "Windows is a non-goal"
grep -q 'POSTHOG_API_KEY' "$W" && fail "telemetry key must not be wired into builds"
grep -q 'package-templates' "$W" && fail "templates are a non-goal"

grep -q 'x86_64-unknown-linux-gnu'  "$W" || fail "missing linux-x64 target"
grep -q 'aarch64-unknown-linux-gnu' "$W" || fail "missing linux-arm64 target"
grep -q 'aarch64-apple-darwin'      "$W" || fail "missing darwin-arm64 target"
grep -q "tags:" "$W" || fail "workflow is not tag-driven"

grep -q '@typed-clickhouse/cli-' "$W" || fail "release.yaml does not reference @typed-clickhouse/cli- packages"
# No @514labs package may be published, installed or depended on. The one
# legitimate mention is the publish-library guard that asserts @514labs/moose-lib
# did NOT survive into dist/, so those lines are excluded; anything else is a
# real regression.
if grep '@514labs' "$W" | grep -qv '@514labs/moose-lib'; then
  grep '@514labs' "$W" | grep -v '@514labs/moose-lib'
  fail "release.yaml still references @514labs"
fi

# The non-tag (dry run) version is written into Cargo.toml and then handed to
# maturin, which converts Cargo versions to PEP 440 before building. SemVer
# prerelease tags like "0.0.0-dryrun" are legal for Cargo but rejected by PEP 440,
# which aborts the build before a single crate compiles. Pin the fallback to a
# plain X.Y.Z, the only shape both schemes accept without translation.
# The rust-cross *-cross* images ship target-prefixed toolchains, so a native
# x86_64 build inside one looks for `x86_64-unknown-linux-gnu-gcc` and finds
# nothing. Let maturin-action choose the image, as the aarch64 leg already does.
grep -q 'manylinux_2_28-cross' "$W" && fail "pinning the rust-cross -cross image breaks the native x86_64 linker"

# sccache never received a compile request (maturin builds in its own container)
# and its post-job step failed on macOS, marking the job failed *after*
# `Publish platform package` had already run — a partial publish that burns the
# version. Not worth re-adding without evidence it caches anything.
grep -q 'sccache-action' "$W" && fail "sccache caches nothing here and its post step can fail a job after publishing"

# before-script-linux runs in two different image families: the Debian-based
# rust-cross image for cross targets and the RHEL-based quay.io/pypa manylinux
# image for native x86_64. A bare `apt install` dies with "apt: command not
# found" on the latter, so the script has to probe for a package manager.
grep -q 'command -v dnf' "$W" || fail "before-script-linux must handle the RHEL-based manylinux image used for native x86_64"

# openssl is vendored, so OpenSSL's Perl ./Configure runs during the build and
# needs IPC::Cmd, which the RHEL-based manylinux image's minimal Perl lacks.
grep -q 'IPC::Cmd' "$W" || fail "before-script-linux must ensure Perl IPC::Cmd for the vendored OpenSSL build"

dryrun_version=$(sed -n 's/^ *VERSION="\([^"$]*\)" *$/\1/p' "$W")
[ -n "$dryrun_version" ] || fail "could not find the non-tag VERSION fallback in release.yaml"
echo "$dryrun_version" | grep -qE '^[0-9]+\.[0-9]+\.[0-9]+$' \
  || fail "dry-run VERSION '$dryrun_version' is not a plain X.Y.Z; maturin will reject it"

# Argument-order contract: release.yaml must invoke release-bin.sh passing
# version, TARGET, OS, NAME in that order (matching release-bin.sh's
# node_version=$1, build_target=$2, build_os=$3, build_name=$4). Swapping the
# TARGET/OS arguments here would not be caught by any other check.
bin_call_block=$(grep -A4 './scripts/release-bin.sh' "$W") || fail "release.yaml does not invoke release-bin.sh"
echo "$bin_call_block" | sed -n '2p' | grep -q 'needs.version.outputs.version' || fail "release-bin.sh call must pass version as arg 1"
echo "$bin_call_block" | sed -n '3p' | grep -q 'matrix.build.TARGET' || fail "release-bin.sh call must pass TARGET as arg 2"
echo "$bin_call_block" | sed -n '4p' | grep -q 'matrix.build.OS' || fail "release-bin.sh call must pass OS as arg 3"
echo "$bin_call_block" | sed -n '5p' | grep -q 'matrix.build.NAME' || fail "release-bin.sh call must pass NAME as arg 4"

# publish-wrapper must depend on build-and-publish-binaries so the platform
# packages exist on the registry before the wrapper is published.
wrapper_block=$(grep -A8 '^  publish-wrapper:' "$W") || fail "publish-wrapper job not found"
echo "$wrapper_block" | grep -q 'needs:' || fail "publish-wrapper job has no needs: block"
echo "$wrapper_block" | grep -q 'build-and-publish-binaries' || fail "publish-wrapper job does not need build-and-publish-binaries"

grep -q 'wait-for-npm-package.sh' "$W" || fail "release.yaml does not wait for npm packages to be available"

# Lockstep. bin.rs compares the CLI's version to `tch-runner print-version`
# with strict equality, so the CLI and the library must ship the same number.
# The only way to guarantee that is for every publishing job to consume the one
# tag-derived value computed in the `version` job, rather than deriving its own.
# This is asserted here rather than in a unit test because Cargo.toml is pinned
# at the "0.0.1" dev sentinel and cannot be compared against the package version.
publish_lib=$(grep -A60 '^  publish-library:' "$W") || fail "publish-library job not found"
echo "$publish_lib" | grep -q 'needs.version.outputs.version' \
  || fail "publish-library must use the shared version, not its own"
echo "$publish_lib" | grep -q "grep -rq '@514labs/moose-lib' dist/" \
  || fail "publish-library must verify no @514labs reference survives into dist"

echo "PASS: release workflow is correctly scoped"
