#!/usr/bin/env bash
# Prints the npm dist-tag a release version publishes under.
#
#   scripts/npm-dist-tag.sh 0.1.3          -> latest
#   scripts/npm-dist-tag.sh 0.2.0-alpha.1  -> next
#
# A SemVer pre-release must never become `latest`, or every plain
# `npm install` would pick up untested code. It goes to `next`, and consumers
# opt in by pinning the exact version.
set -euo pipefail

version="${1:?usage: npm-dist-tag.sh <version>}"

if [[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  echo latest
elif [[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+-[0-9A-Za-z.-]+$ ]]; then
  echo next
else
  echo "not a SemVer release version: $version" >&2
  exit 1
fi
