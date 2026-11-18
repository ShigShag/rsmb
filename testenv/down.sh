#!/usr/bin/env bash
# Stops and removes the test environment's containers and networks. Images stay, so the next
# up.sh starts in seconds.
set -euo pipefail
cd "$(dirname "$0")"
docker compose down --remove-orphans --timeout 2
