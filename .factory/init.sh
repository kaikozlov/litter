#!/usr/bin/env bash
set -euo pipefail

# Ensure cargo is from rustup, not Homebrew
if command -v rustup &>/dev/null; then
    export PATH="$(rustup which cargo | xargs dirname):$PATH"
fi

echo "Environment initialized. cargo=$(which cargo), rustc=$(rustc --version)"
