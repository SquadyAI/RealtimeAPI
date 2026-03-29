#!/usr/bin/env bash
# ============================================================================
# SquadyAI Realtime API — Start Command
# Usage: source this from the `realtime` entry script
#   cmd_start [--reconfigure]
# ============================================================================

set -euo pipefail

cmd_start() {
    local reconfigure=false

    while [[ $# -gt 0 ]]; do
        case "$1" in
            --reconfigure) reconfigure=true; shift ;;
            *)             shift ;;
        esac
    done

    # ── No .env → auto-onboard ────────────────────────────────────────────
    if [[ ! -f "$ENV_FILE" ]]; then
        echo ""
        msg "  ${YELLOW}${SYM_WARN}${NC} No .env file found. Starting setup wizard..." \
            "  ${YELLOW}${SYM_WARN}${NC} 未找到 .env 文件。启动设置向导..."
        echo ""
        cmd_onboard
        # After onboard, continue to start
    fi

    # ── --reconfigure → run onboard first ─────────────────────────────────
    if [[ "$reconfigure" == "true" ]]; then
        cmd_onboard --force
    fi

    # ── Load language ─────────────────────────────────────────────────────
    load_ui_lang

    # ── Pre-flight Checks ─────────────────────────────────────────────────
    echo ""
    msg "  ${BOLD}SquadyAI Realtime API${NC}" \
        "  ${BOLD}SquadyAI Realtime API${NC}"
    echo ""

    print_ok "$(msg '.env loaded' '.env 已加载')"

    # Check required config (LLM_BASE_URL and LLM_MODEL are required; LLM_API_KEY is optional for self-hosted LLMs)
    if ! require_env "LLM_BASE_URL"; then
        exit 1
    fi
    if ! require_env "LLM_MODEL"; then
        exit 1
    fi

    # ── Config summary (includes connectivity checks) ──────────────────
    print_config_summary

    # ── Find binary (installed or locally built) ────────────────────────
    local BINARY=""

    # 1. Locally built binary (project dir)
    if [[ -f "${PROJECT_DIR}/target/release/realtime" ]]; then
        BINARY="${PROJECT_DIR}/target/release/realtime"
    # 2. Installed binary (alongside CLI scripts)
    elif [[ -f "${REALTIME_HOME}/realtime-server" ]]; then
        BINARY="${REALTIME_HOME}/realtime-server"
    fi

    if [[ -z "$BINARY" ]]; then
        # Check build dependencies before attempting build
        if ! has_cmd cmake; then
            print_err "$(msg 'cmake is required to build but not installed.' 'cmake 是构建必需依赖但未安装。')"
            echo ""
            if [[ "$(uname)" == "Darwin" ]]; then
                msg "  Install: ${BOLD}brew install cmake${NC}" \
                    "  安装: ${BOLD}brew install cmake${NC}"
            else
                msg "  Install: ${BOLD}sudo apt install cmake${NC} or ${BOLD}sudo yum install cmake${NC}" \
                    "  安装: ${BOLD}sudo apt install cmake${NC} 或 ${BOLD}sudo yum install cmake${NC}"
            fi
            echo ""
            exit 1
        fi
        if has_cmd cargo; then
            print_warn "$(msg 'Release binary not found. Building...' 'Release 二进制未找到。正在构建...')"
            echo ""
            msg "  Running: cargo build --release" \
                "  执行: cargo build --release"
            echo ""
            cd "$PROJECT_DIR"
            # Fix cmake 4.x compatibility with older CMakeLists.txt (audiopus_sys)
            export CMAKE_POLICY_VERSION_MINIMUM=3.5
            cargo build --release
            echo ""
            print_ok "$(msg 'Build complete!' '构建完成！')"
            BINARY="${PROJECT_DIR}/target/release/realtime"
        else
            print_err "$(msg 'Release binary not found and cargo is not installed.' 'Release 二进制未找到且 cargo 未安装。')"
            msg "  $(msg 'Build first: cargo build --release' '请先构建: cargo build --release')" \
                "  请先构建: cargo build --release"
            exit 1
        fi
    fi

    # ── Version Check ─────────────────────────────────────────────────────
    check_update 2>/dev/null || true

    # ── Start ─────────────────────────────────────────────────────────────
    local BIND_ADDR PORT ENABLE_TTS_VAL TTS_ENGINE_VAL LLM_MODEL
    BIND_ADDR=$(env_get "BIND_ADDR" "0.0.0.0:8080")
    PORT=$(echo "$BIND_ADDR" | grep -o '[0-9]*$')
    ENABLE_TTS_VAL=$(env_get "ENABLE_TTS" "false")
    TTS_ENGINE_VAL=$(env_get "TTS_ENGINE" "none")
    LLM_MODEL=$(env_get "LLM_MODEL" "unknown")

    echo ""
    msg "  ${BOLD}Starting Realtime API...${NC}" \
        "  ${BOLD}启动 Realtime API...${NC}"
    echo ""
    echo -e "  ${GREEN}${BOLD}  ★  Playground:  http://localhost:${PORT}${NC}"
    echo ""
    echo -e "  ┌──────────────────────────────────────────────┐"
    echo -e "  │  WebSocket: ws://${BIND_ADDR}/ws             │"
    echo -e "  │  Health:    http://${BIND_ADDR}/health       │"
    echo -e "  │  Metrics:   http://${BIND_ADDR}/metrics      │"
    echo -e "  │                                              │"
    echo -e "  │  LLM:  ${LLM_MODEL}                         │"
    if [[ "$ENABLE_TTS_VAL" == "true" ]]; then
        echo -e "  │  TTS:  ${TTS_ENGINE_VAL}                     │"
    else
        echo -e "  │  TTS:  disabled                              │"
    fi
    echo -e "  └──────────────────────────────────────────────┘"
    echo ""

    # Load .env and run
    set -a
    source "$ENV_FILE"
    set +a

    # Logs go to file, terminal stays clean (like OpenClaw)
    local LOG_DIR="${PROJECT_DIR}/logs"
    mkdir -p "$LOG_DIR"
    local LOG_FILE="${LOG_DIR}/realtime.log"

    msg "  $(printf 'Logs: %s' "$LOG_FILE")" \
        "  $(printf '日志: %s' "$LOG_FILE")"
    msg "  $(printf 'Tail logs: tail -f %s' "$LOG_FILE")" \
        "  $(printf '查看日志: tail -f %s' "$LOG_FILE")"
    echo ""
    msg "  ${DIM}Press Ctrl+C to stop${NC}" \
        "  ${DIM}按 Ctrl+C 停止服务${NC}"
    echo ""

    # Run in foreground but redirect logs to file
    # Trap SIGINT/SIGTERM to cleanly stop
    "$BINARY" >> "$LOG_FILE" 2>&1 &
    local SERVER_PID=$!

    trap "kill $SERVER_PID 2>/dev/null; wait $SERVER_PID 2>/dev/null; exit 0" INT TERM

    # Wait a moment then check if server started OK
    sleep 3
    if ! kill -0 "$SERVER_PID" 2>/dev/null; then
        echo ""
        print_err "$(msg 'Server failed to start. Check logs:' '服务启动失败。查看日志：')"
        echo "  tail -20 $LOG_FILE"
        echo ""
        tail -5 "$LOG_FILE"
        exit 1
    fi

    print_ok "$(msg 'Server is running!' '服务已启动！')"
    echo ""

    # Block until server exits
    wait "$SERVER_PID"
}
