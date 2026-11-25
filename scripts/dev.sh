#!/usr/bin/env bash
# Run the Purrnel harness on a dev machine.
# whisper.cpp comes from nixpkgs; models live in ./models (see PLAN.md).
set -euo pipefail
cd "$(dirname "$0")/.."
export PURRNEL_ROOT="$PWD"
if [ -z "${PURRNEL_WHISPER_BIN:-}" ] && ! command -v whisper-server >/dev/null; then
  out=$(nix --extra-experimental-features 'nix-command flakes' build nixpkgs#whisper-cpp --no-link --print-out-paths | head -1)
  export PURRNEL_WHISPER_BIN="$out/bin/whisper-server"
fi
echo "purrnel: building (the first run after a change can take a minute or two; later starts are quick)..." >&2
exec cargo run -p purrnel-harness
