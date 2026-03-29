#!/usr/bin/env bash
# Thin wrapper — delegates to ./realtime doctor
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
exec "${SCRIPT_DIR}/realtime" doctor "$@"
