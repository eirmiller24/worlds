#!/usr/bin/env bash
# Runs the Worlds Godot addon tests headless: bakes tests/fixtures/game,
# then checks it and every fixture export of worlds-cli check-export.
#
#   godot/tests/run.sh            # uses `godot` from PATH, or $GODOT
set -euo pipefail

repo="$(cd "$(dirname "$0")/../.." && pwd)"
godot="${GODOT:-godot}"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

cargo run -q --release -p worlds-cli --manifest-path "$repo/Cargo.toml" -- \
  bake "$repo/tests/fixtures/game/game.worlds" --out "$tmp/game_export" >/dev/null

dirs=()
for case in "$repo"/crates/worlds-cli/tests/fixtures/exports/*/; do
  dirs+=("${case%/}")
done
dirs+=("$tmp/game_export")

# Register the addon's classes (class_name) before running the script.
"$godot" --headless --path "$repo/godot" --import >/dev/null 2>&1
"$godot" --headless --path "$repo/godot" -s tests/test_import.gd -- "${dirs[@]}"
