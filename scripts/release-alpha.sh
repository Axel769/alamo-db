#!/bin/bash
# Alpha release from the Mac: macOS build (universal, signed with the personal
# Apple Development certificate, see below) and
# Windows build (NSIS installer compiled from macOS with cargo-xwin), published
# as a GitHub release of THIS repo (tag v<version>, assets, latest.json for
# the updater). Up to 0.1.0-alpha.12 the builds lived in
# Axel769/alamo-db-releases (the source was private): that repo gets, once
# per release until it is archived, a release carrying only a latest.json
# that points here, so that older installations still find the update.
#
# The keys stay on this Mac (~/.tauri): no secrets on GitHub runners or on
# the VPS. Beforehand: merge into main, "version" bumped in
# app/src-tauri/tauri.conf.json, section for the version in CHANGELOG.md.
#
# Tools: gh, jq, pnpm, rustup with the targets aarch64-apple-darwin,
# x86_64-apple-darwin and x86_64-pc-windows-msvc; for Windows
# `brew install nsis llvm lld` and `cargo install --locked cargo-xwin`.
#
# Usage: scripts/release-alpha.sh [--no-publish] [--app DIR --config FILE]
#   --no-publish  builds and verifies, publishes nothing (no release, no tag).
#   --app DIR     builds the extension app in DIR (ARCHITECTURE §13) instead
#                 of app/, with the release configuration FILE (absolute):
#                 same identifier, version and update channel as this one,
#                 so it is published here exactly like the Community build.
#                 DIR is its own Cargo workspace; its frontend is built.
set -euo pipefail

publish=1
app_dir=""
release_conf=""
while [ $# -gt 0 ]; do
  case "$1" in
    --no-publish) publish=0 ;;
    --app) app_dir="${2:?--app needs a folder}"; shift ;;
    --config) release_conf="${2:?--config needs a file}"; shift ;;
    *) echo "release: unknown option $1" >&2; exit 1 ;;
  esac
  shift
done

root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"
repo=Axel769/alamo-db
# Updater endpoint of the installations up to alpha.12 (see the header).
bridge=Axel769/alamo-db-releases
keys="$HOME/.tauri"
if [ -n "$app_dir" ]; then
  [ -n "$release_conf" ] || { echo "release: --app needs --config" >&2; exit 1; }
  build_dir="$app_dir"
  target="${CARGO_TARGET_DIR:-$app_dir/target}"
else
  build_dir="$root/app"
  release_conf="$root/app/src-tauri/tauri.release.conf.json"
  target="${CARGO_TARGET_DIR:-$root/target}"
fi
tauri="$root/app/node_modules/.bin/tauri"
# Homebrew's llvm and lld are keg-only: llvm-rc, clang-cl and lld-link for
# cargo-xwin live here. `cargo install` puts cargo-xwin in ~/.cargo/bin.
export PATH="/opt/homebrew/opt/llvm/bin:/opt/homebrew/opt/lld/bin:$PATH:$HOME/.cargo/bin"

die() { echo "release: $*" >&2; exit 1; }

for tool in gh jq pnpm cargo-xwin makensis llvm-rc lld-link; do
  command -v "$tool" >/dev/null || die "missing $tool"
done
for f in alamo-updater.key alamo-sign-identity; do
  [ -f "$keys/$f" ] || die "missing $keys/$f"
done

# --- What gets released: the content of main, new version and notes.
git fetch -q origin main --tags
commit="$(git rev-parse origin/main)"
if [ "$publish" = 1 ]; then
  [ -z "$(git status --porcelain)" ] || die "uncommitted changes"
  git diff --quiet origin/main HEAD -- \
    || die "the checkout does not match origin/main: merge and update"
elif ! git diff --quiet origin/main HEAD --; then
  # The dry run can be done before the merge: the checkout is built as is.
  commit="$(git rev-parse HEAD)"
  echo "== dry run on the current checkout, not on origin/main"
fi
version="$(jq -r .version app/src-tauri/tauri.conf.json)"
tag="v$version"
if git rev-parse -q --verify "refs/tags/$tag" >/dev/null; then
  # A dry run builds whatever is checked out: only a publish needs a new tag.
  [ "$publish" = 0 ] || die "$tag already exists"
  echo "== $tag already exists: fine for a dry run, a publish needs a new version"
fi
notes="What's new in this version:
$(.github/scripts/release-notes.sh "$version")"

echo "== Alamo DB $version from $commit"
echo "$notes"

# Security advisories come out every day: today's check.
cargo deny --all-features check bans licenses sources advisories 2>/dev/null \
  || die "cargo deny is not green"

# --- macOS signing: Apple Development certificate of the personal team (Xcode,
# free), the same as `cargo xtask run`. Keychain remembers
# "Always Allow" per TEAM: with the old self-signed certificate
# "Alamo DB" macOS only remembered the fingerprint (cdhash) of the single build
# and asked for the password at every version. The name lives in
# ~/.tauri/alamo-sign-identity (contains the email, outside the repo); it must
# match exactly one valid identity, or codesign refuses it.
identity="$(tr -d '\n' < "$keys/alamo-sign-identity")"
matches="$(security find-identity -v -p codesigning | grep -cF "\"$identity\"" || true)"
[ "$matches" = 1 ] || die "signing identity \"$identity\": $matches valid, exactly one needed"

# Updater signature (not the code signature): without it, the installed app
# refuses the update.
export TAURI_SIGNING_PRIVATE_KEY="$(cat "$keys/alamo-updater.key")"
export TAURI_SIGNING_PRIVATE_KEY_PASSWORD=""

(cd app && pnpm install --frozen-lockfile)

echo "== macOS build (universal) of $build_dir"
(cd "$build_dir" && APPLE_SIGNING_IDENTITY="$identity" "$tauri" build \
  --target universal-apple-darwin --bundles app,dmg \
  --config "$release_conf")
mac="$target/universal-apple-darwin/release/bundle"
app="$mac/macos/Alamo DB.app"
codesign -v --strict "$app"
codesign -dr - "$app" 2>&1 | grep -qF "identifier \"io.github.axel769.alamo\" and anchor apple generic and certificate leaf[subject.CN] = \"$identity\"" \
  || die "the .app does not carry the signature \"$identity\""

echo "== Windows build (NSIS, cargo-xwin)"
(cd "$build_dir" && "$tauri" build --runner cargo-xwin \
  --target x86_64-pc-windows-msvc --bundles nsis \
  --config "$release_conf")
win="$target/x86_64-pc-windows-msvc/release/bundle/nsis"

# --- Assets with the names of the previous releases (made by tauri-action).
out="$(mktemp -d)"
cp "$mac/macos/Alamo DB.app.tar.gz" "$out/Alamo.DB_${version}_universal.app.tar.gz"
cp "$mac/macos/Alamo DB.app.tar.gz.sig" "$out/Alamo.DB_${version}_universal.app.tar.gz.sig"
cp "$mac/dmg/Alamo DB_${version}_universal.dmg" "$out/Alamo.DB_${version}_universal.dmg"
cp "$win/Alamo DB_${version}_x64-setup.exe" "$out/Alamo.DB_${version}_x64-setup.exe"
cp "$win/Alamo DB_${version}_x64-setup.exe.sig" "$out/Alamo.DB_${version}_x64-setup.exe.sig"
ls -lh "$out"

if [ "$publish" = 0 ]; then
  echo "== --no-publish: assets in $out, nothing published"
  exit 0
fi

echo "== publishing to $repo"
# Draft until everything is there: the updater reads releases/latest and must
# not see a release without latest.json. The tag is created on the commit
# when the draft is published.
printf '%s\n' "$notes" > "$out/notes.txt"
gh release create "$tag" -R "$repo" --draft --target "$commit" \
  --title "Alamo DB $version" --notes-file "$out/notes.txt" \
  "$out"/Alamo.DB_*

# Public download URLs, not the API ones: they stay valid for any client,
# with no Accept header, and the bridge manifest can carry them as they are.
mac_asset="Alamo.DB_${version}_universal.app.tar.gz"
win_asset="Alamo.DB_${version}_x64-setup.exe"
uploaded="$(gh release view "$tag" -R "$repo" --json assets -q '.assets[].name')"
for a in "$mac_asset" "$win_asset"; do
  grep -qxF "$a" <<< "$uploaded" || die "asset $a not uploaded"
done
mac_url="https://github.com/$repo/releases/download/$tag/$mac_asset"
win_url="https://github.com/$repo/releases/download/$tag/$win_asset"

# Same platform keys as tauri-action.
jq -n \
  --arg version "$version" \
  --arg notes "$notes" \
  --arg date "$(date -u +%Y-%m-%dT%H:%M:%S.000Z)" \
  --rawfile mac_sig "$out/Alamo.DB_${version}_universal.app.tar.gz.sig" \
  --rawfile win_sig "$out/Alamo.DB_${version}_x64-setup.exe.sig" \
  --arg mac_url "$mac_url" \
  --arg win_url "$win_url" \
  '{signature: ($win_sig | rtrimstr("\n")), url: $win_url} as $w
   | {signature: ($mac_sig | rtrimstr("\n")), url: $mac_url} as $m
   | {version: $version, notes: $notes, pub_date: $date,
      platforms: {
        "windows-x86_64": $w, "windows-x86_64-nsis": $w,
        "darwin-aarch64": $m, "darwin-x86_64": $m, "darwin-universal": $m,
        "darwin-aarch64-app": $m, "darwin-x86_64-app": $m,
        "darwin-universal-app": $m}}' > "$out/latest.json"
gh release upload "$tag" -R "$repo" "$out/latest.json"
# Not a prerelease: releases/latest (the updater endpoint) skips those. And
# explicitly the latest: GitHub otherwise ranks releases by the date of the
# commit they point at, not by publication.
gh release edit "$tag" -R "$repo" --draft=false --prerelease=false --latest
git fetch -q origin --tags
echo "== published: https://github.com/$repo/releases/tag/$tag"

# The bridge: same latest.json (URLs already point here) on the old endpoint,
# so that installations up to alpha.12 update to this version and from then
# on follow the new endpoint. Skipped once that repo is archived.
if gh api "repos/$bridge" -q .archived | grep -qx false; then
  echo "== bridge manifest on $bridge"
  printf '%s\n' \
    "Alamo DB is now released from https://github.com/$repo/releases." \
    "" \
    "This release carries only the updater manifest that points there, for" \
    "installations up to 0.1.0-alpha.12. Download the app from the link above." \
    > "$out/bridge-notes.txt"
  # --latest matters here: every release of that repo points at the same
  # commit of its main, so the ranking by commit date is a tie.
  gh release create "$tag" -R "$bridge" --target main --latest \
    --title "Alamo DB $version (moved to $repo)" --notes-file "$out/bridge-notes.txt" \
    "$out/latest.json"
fi
