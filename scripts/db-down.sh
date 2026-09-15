#!/usr/bin/env bash
# Stop both databases. Pass -v to drop the data volumes as well.
set -euo pipefail
cd "$(dirname "$0")/.."
docker compose down "$@"
