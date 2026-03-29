#!/usr/bin/env bash
# Thin wrapper — delegates to ./realtime onboard
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
exec "${SCRIPT_DIR}/realtime" onboard "$@"
