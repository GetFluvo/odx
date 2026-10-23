#!/usr/bin/env bash
# Prepare the fixture project of the bot cycle job: a git repository whose
# origin is the fake forge, locked and committed, in "$1".
set -euo pipefail
fixture="$(cd "$(dirname "$0")/../tests/fixtures/bot" && pwd)"
mkdir -p "$1"
cp "$fixture/pyproject.toml" "$1/"
cd "$1"
git init -q -b main
git remote add origin "http://127.0.0.1:${FORGE_PORT}/acme/project.git"
odx lock
git add -A
git -c user.name=odx -c user.email=odx@example.com commit -q -m "fixture project"
