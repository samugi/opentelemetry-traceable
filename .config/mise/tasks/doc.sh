#!/usr/bin/env bash
# [MISE] description="Docs"

set -euo pipefail

cargo doc --locked --workspace --keep-going --no-deps
