#!/usr/bin/env bash
# [MISE] description="Run rustfmt formatter"

set -euo pipefail

cargo fmt --all -- --check
