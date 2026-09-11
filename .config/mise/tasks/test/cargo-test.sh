#!/usr/bin/env bash
# [MISE] description="Run tests using Cargo Nextest"

set -euo pipefail

cargo nextest run --locked --workspace
