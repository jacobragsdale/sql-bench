#!/usr/bin/env bash
# Throw both databases away and build them again from scratch.
set -euo pipefail
cd "$(dirname "$0")"
./db-down.sh -v
./db-up.sh
