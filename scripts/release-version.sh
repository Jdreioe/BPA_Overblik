#!/usr/bin/env bash
# The single source of the release version: the tag being built.
#
# A release is dated, not numbered: YYYY.MM.DD. The release workflow exports
# TEAMUP_SHIFT_SYNC_VERSION from the pushed tag; a local packaging run without
# it is dated today, so nothing has to be edited to try a package by hand.
set -euo pipefail

version="${TEAMUP_SHIFT_SYNC_VERSION:-$(date -u +%Y.%m.%d)}"

if ! printf '%s' "$version" | grep -Eq '^[0-9]{4}\.[0-9]{2}\.[0-9]{2}$'; then
  echo "Release version must be YYYY.MM.DD, got: $version" >&2
  exit 1
fi

printf '%s' "$version"
