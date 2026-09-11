#!/usr/bin/env bash
# [MISE] description="Build"

set -euo pipefail

cargo build --locked --workspace
