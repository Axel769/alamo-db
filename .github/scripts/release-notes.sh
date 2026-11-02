#!/usr/bin/env bash
# Release notes of version $1 from CHANGELOG.md: the lines between "## [$1]"
# and the next section, without blank lines at the edges. Exits with an error
# if the section is missing or empty: the release must not start without notes.
set -euo pipefail
version="${1:?usage: release-notes.sh <version>}"
notes="$(awk -v v="$version" '
  index($0, "## [" v "]") == 1 { on = 1; next }
  on && /^## / { exit }
  on { print }
' CHANGELOG.md | sed -e '/./,$!d')"  # $(...) already strips the trailing blank lines
if [ -z "$notes" ]; then
  echo "CHANGELOG.md: section \"## [$version]\" is missing (or empty)" >&2
  exit 1
fi
printf '%s\n' "$notes"
