#!/usr/bin/env bash
# Starts the app in development: frontend build + build/sign alamo-app + launch.
# See "Running the app" in the README.
set -euo pipefail

cd "$(dirname "$0")"

# Frontend dependencies only if missing (first run).
if [ ! -d app/node_modules ]; then
  (cd app && pnpm install)
fi

exec cargo xtask run "$@"
