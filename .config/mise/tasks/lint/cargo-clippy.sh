#!/usr/bin/env bash
# [MISE] description="Run Clippy Lints"

set -euo pipefail

cargo clippy --locked --workspace --all-targets --all-features -- \
    -D warnings \
    -W missing_debug_implementations \
    -W missing_docs \
    -W unsafe_code \
    -W clippy::undocumented_unsafe_blocks
