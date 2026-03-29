#!/usr/bin/env bash
# ============================================================================
# SquadyAI Realtime API — Doctor (Health Diagnostics)
# Usage: source this from the `realtime` entry script
#   cmd_doctor [--lang zh/en]
# ============================================================================

set -euo pipefail

cmd_doctor() {
    local lang_arg=""

    while [[ $# -gt 0 ]]; do
        case "$1" in
            --lang) lang_arg="$2"; shift 2 ;;
            *)      shift ;;
        esac
    done

    # ── Language ──────────────────────────────────────────────────────────
    if [[ -n "$lang_arg" ]]; then
        load_ui_lang --lang "$lang_arg"
    else
        load_ui_lang
    fi

    print_banner

    echo ""
    echo -e "${CYAN}━━━ $(msg 'SquadyAI Realtime API — Health Diagnostics' 'SquadyAI Realtime API — 健康诊断') ━━━${NC}"

    local WARNINGS=0
    local ERRORS=0

    # ── Environment Check ────────────────────────────────────────────────
    echo ""
    msg "${BOLD}  Environment:${NC}" "${BOLD}  环境检查:${NC}"

    # Rust
    if has_cmd rustc; then
        local rust_ver
        rust_ver=$(rustc --version 2>/dev/null | head -1)
        if echo "$rust_ver" | grep -qi "nightly"; then
            print_ok "Rust nightly installed (${rust_ver})"
        else
            print_warn "$(msg "Rust installed but not nightly: ${rust_ver}. Run: rustup default nightly" "Rust 已安装但非 nightly: ${rust_ver}。运行: rustup default nightly")"
            ((WARNINGS++))
        fi
    else
        print_err "$(msg 'Rust not installed. Visit: https://rustup.rs/' 'Rust 未安装。访问: https://rustup.rs/')"
        ((ERRORS++))
    fi

    # cargo
    if has_cmd cargo; then
        print_ok "cargo $(cargo --version 2>/dev/null | awk '{print $2}')"
    else
        print_err "$(msg 'cargo not found' 'cargo 未找到')"
        ((ERRORS++))
    fi

    # .env file
    if [[ -f "$ENV_FILE" ]]; then
        print_ok "$(msg '.env file exists' '.env 文件存在')"
    else
        print_err "$(msg '.env file not found. Run: ./realtime onboard' '.env 文件不存在。运行: ./realtime onboard')"
        ((ERRORS++))
    fi

    # Port check
    local PORT
    PORT=$(env_get "BIND_ADDR" "0.0.0.0:8080" | grep -o '[0-9]*$')
    if [[ -n "$PORT" ]]; then
        if check_port_available "$PORT"; then
            print_ok "$(msg "Port ${PORT} is available" "端口 ${PORT} 可用")"
        else
            print_warn "$(msg "Port ${PORT} is already in use" "端口 ${PORT} 已被占用")"
            ((WARNINGS++))
        fi
    fi

    # ── Required Services ────────────────────────────────────────────────
    echo ""
    msg "${BOLD}  Required Services:${NC}" "${BOLD}  必需服务:${NC}"

    # LLM API
    local LLM_URL LLM_KEY LLM_MDL
    LLM_URL=$(env_get "LLM_BASE_URL" "")
    LLM_KEY=$(env_get "LLM_API_KEY" "")
    LLM_MDL=$(env_get "LLM_MODEL" "")

    if [[ -z "$LLM_URL" || -z "$LLM_KEY" ]]; then
        print_err "$(msg 'LLM not configured. Run: ./realtime onboard' 'LLM 未配置。运行: ./realtime onboard')"
        ((ERRORS++))
    elif [[ "$LLM_KEY" == "sk-your-api-key-here" ]]; then
        print_err "$(msg 'LLM API Key is placeholder. Run: ./realtime onboard' 'LLM API Key 是占位符。运行: ./realtime onboard')"
        ((ERRORS++))
    else
        local start_time end_time
        start_time=$(date +%s%N 2>/dev/null || date +%s)
        if validate_llm "$LLM_URL" "$LLM_KEY" "$LLM_MDL"; then
            end_time=$(date +%s%N 2>/dev/null || date +%s)
            if [[ "$start_time" =~ ^[0-9]+$ && "$end_time" =~ ^[0-9]+$ && ${#start_time} -gt 10 ]]; then
                local latency_ms=$(( (end_time - start_time) / 1000000 ))
                print_ok "$(msg "LLM API reachable (${LLM_URL}, ${latency_ms}ms)" "LLM API 可达 (${LLM_URL}, ${latency_ms}ms)")"
            else
                print_ok "$(msg "LLM API reachable (${LLM_URL})" "LLM API 可达 (${LLM_URL})")"
            fi
        else
            print_warn "$(msg "LLM API not reachable at ${LLM_URL}. Check URL and API key." "LLM API 无法连接 ${LLM_URL}。请检查地址和 API Key。")"
            ((WARNINGS++))
        fi
    fi

    # ── Optional Services ────────────────────────────────────────────────
    echo ""
    msg "${BOLD}  Optional Services:${NC}" "${BOLD}  可选服务:${NC}"

    # TTS
    local ENABLE_TTS_VAL TTS_ENGINE_VAL
    ENABLE_TTS_VAL=$(env_get "ENABLE_TTS" "false")
    TTS_ENGINE_VAL=$(env_get "TTS_ENGINE" "")

    if [[ "$ENABLE_TTS_VAL" == "true" ]]; then
        case "$TTS_ENGINE_VAL" in
            edge)
                print_ok "$(msg 'TTS engine: Edge TTS (free, no validation needed)' 'TTS 引擎: Edge TTS（免费，无需验证）')"
                ;;
            azure)
                local azure_key
                azure_key=$(env_get "AZURE_SPEECH_KEY" "")
                if [[ -n "$azure_key" ]]; then
                    print_ok "$(msg 'TTS engine: Azure Speech (key configured)' 'TTS 引擎: Azure 语音（Key 已配置）')"
                else
                    print_warn "$(msg 'Azure Speech key not set' 'Azure Speech Key 未设置')"
                    ((WARNINGS++))
                fi
                ;;
            minimax)
                local mm_key
                mm_key=$(env_get "MINIMAX_API_KEY" "")
                if [[ -n "$mm_key" && "$mm_key" != "your_minimax_api_key_here" ]]; then
                    print_ok "$(msg 'TTS engine: MiniMax (key configured)' 'TTS 引擎: MiniMax（Key 已配置）')"
                else
                    print_warn "$(msg 'MiniMax API key not set or is placeholder' 'MiniMax API Key 未设置或为占位符')"
                    ((WARNINGS++))
                fi
                ;;
            volc)
                local volc_id volc_token
                volc_id=$(env_get "VOLC_APP_ID" "")
                volc_token=$(env_get "VOLC_ACCESS_TOKEN" "")
                if [[ -n "$volc_id" && -n "$volc_token" ]]; then
                    print_ok "$(msg 'TTS engine: VolcEngine (configured)' 'TTS 引擎: 火山引擎（已配置）')"
                else
                    print_warn "$(msg 'VolcEngine credentials incomplete' '火山引擎凭证不完整')"
                    ((WARNINGS++))
                fi
                ;;
            baidu)
                local baidu_key
                baidu_key=$(env_get "BAIDU_TTS_API_KEY" "")
                if [[ -n "$baidu_key" ]]; then
                    print_ok "$(msg 'TTS engine: Baidu (key configured)' 'TTS 引擎: 百度（Key 已配置）')"
                else
                    print_warn "$(msg 'Baidu TTS key not set' '百度 TTS Key 未设置')"
                    ((WARNINGS++))
                fi
                ;;
            *)
                print_warn "$(msg "Unknown TTS engine: ${TTS_ENGINE_VAL}" "未知 TTS 引擎: ${TTS_ENGINE_VAL}")"
                ((WARNINGS++))
                ;;
        esac
    else
        print_info "$(msg 'TTS disabled — text-only mode' 'TTS 已禁用 — 纯文本模式')"
    fi

    # SearXNG
    local SEARX_URL
    SEARX_URL=$(env_get "SEARXNG_BASE_URL" "")
    if [[ -n "$SEARX_URL" ]]; then
        if check_http "${SEARX_URL}" 3 || check_http "${SEARX_URL}/healthz" 3; then
            print_ok "$(msg "SearXNG reachable (${SEARX_URL})" "SearXNG 可达 (${SEARX_URL})")"
        else
            print_warn "$(msg "SearXNG not reachable at ${SEARX_URL}" "SearXNG 无法连接 ${SEARX_URL}")"
            ((WARNINGS++))
        fi
    else
        print_info "$(msg 'SearXNG not configured — search disabled' 'SearXNG 未配置 — 搜索功能不可用')"
    fi

    # Intent API
    local INTENT_URL
    INTENT_URL=$(env_get "INTENT_API_URL" "")
    if [[ -n "$INTENT_URL" ]]; then
        if check_http "${INTENT_URL}" 3; then
            print_ok "$(msg "Intent API reachable (${INTENT_URL})" "意图识别 API 可达 (${INTENT_URL})")"
        else
            print_warn "$(msg "Intent API not reachable at ${INTENT_URL}" "意图识别 API 无法连接 ${INTENT_URL}")"
            ((WARNINGS++))
        fi
    else
        print_info "$(msg 'Intent API not configured' '意图识别 API 未配置')"
    fi

    # SimulInterpret
    local SIMUL_VAL
    SIMUL_VAL=$(env_get "ENABLE_SIMUL_INTERPRET" "false")
    if [[ "$SIMUL_VAL" == "true" ]]; then
        print_ok "$(msg 'Simultaneous interpretation enabled' '同声传译已启用')"
    else
        print_info "$(msg 'Simultaneous interpretation disabled' '同声传译已禁用')"
    fi

    # Vision LLM
    local VISION_URL
    VISION_URL=$(env_get "VISUAL_LLM_STREAM_URL" "")
    if [[ -n "$VISION_URL" ]]; then
        # Extract base URL for health check
        local vision_host
        vision_host=$(echo "$VISION_URL" | sed 's|https\?://||' | cut -d/ -f1)
        if curl -sf --max-time 3 --head "http://${vision_host}" &>/dev/null || \
           curl -sf --max-time 3 --head "https://${vision_host}" &>/dev/null; then
            print_ok "$(msg "Vision LLM reachable (${vision_host})" "视觉 LLM 可达 (${vision_host})")"
        else
            print_warn "$(msg "Vision LLM not reachable at ${vision_host}" "视觉 LLM 无法连接 ${vision_host}")"
            ((WARNINGS++))
        fi
    else
        print_info "$(msg 'Vision LLM not configured' '视觉 LLM 未配置')"
    fi

    # WhisperLive
    local WHISPER_PATH
    WHISPER_PATH=$(env_get "WHISPERLIVE_PATH" "")
    if [[ -n "$WHISPER_PATH" ]]; then
        # WebSocket URL — try basic HTTP connectivity on the host
        local whisper_host
        whisper_host=$(echo "$WHISPER_PATH" | sed 's|wss\?://||' | cut -d/ -f1)
        if curl -sf --max-time 3 --head "http://${whisper_host}" &>/dev/null 2>&1; then
            print_ok "$(msg "WhisperLive host reachable (${whisper_host})" "WhisperLive 主机可达 (${whisper_host})")"
        else
            print_warn "$(msg "WhisperLive host not reachable at ${whisper_host}" "WhisperLive 主机无法连接 ${whisper_host}")"
            ((WARNINGS++))
        fi
    else
        print_info "$(msg 'WhisperLive not configured — ASR will not be available' 'WhisperLive 未配置 — ASR 将不可用')"
    fi

    # PostgreSQL
    local DB_URL
    DB_URL=$(env_get "DATABASE_URL" "")
    if [[ -n "$DB_URL" ]]; then
        if has_cmd psql; then
            if psql "$DB_URL" -c "SELECT 1" &>/dev/null; then
                print_ok "$(msg 'PostgreSQL connection successful' 'PostgreSQL 连接成功')"
            else
                print_warn "$(msg 'PostgreSQL connection failed' 'PostgreSQL 连接失败')"
                ((WARNINGS++))
            fi
        else
            print_info "$(msg "PostgreSQL configured (${DB_URL%@*}@...) — psql not available for testing" "PostgreSQL 已配置 — psql 不可用，无法测试连接")"
        fi
    else
        print_info "$(msg 'PostgreSQL not configured — in-memory mode' 'PostgreSQL 未配置 — 使用内存模式')"
    fi

    # Langfuse
    local LF_KEY
    LF_KEY=$(env_get "LANGFUSE_SECRET_KEY" "")
    if [[ -n "$LF_KEY" ]]; then
        print_ok "$(msg 'Langfuse configured' 'Langfuse 已配置')"
    else
        print_info "$(msg 'Langfuse not configured — no request tracing' 'Langfuse 未配置 — 无请求追踪')"
    fi

    # ── Model Files ──────────────────────────────────────────────────────
    echo ""
    msg "${BOLD}  Model Files:${NC}" "${BOLD}  模型文件:${NC}"

    # VAD model (Silero)
    local VAD_MODEL_PATHS=(
        "${PROJECT_DIR}/silero_vad.onnx"
        "${PROJECT_DIR}/models/silero_vad.onnx"
        "${PROJECT_DIR}/src/vad/silero_vad.onnx"
    )
    local vad_found=false
    for vp in "${VAD_MODEL_PATHS[@]}"; do
        if [[ -f "$vp" ]]; then
            print_ok "$(msg "Silero VAD model found (${vp})" "Silero VAD 模型已找到 (${vp})")"
            vad_found=true
            break
        fi
    done
    if [[ "$vad_found" == "false" ]]; then
        print_info "$(msg 'Silero VAD model not found locally — will be downloaded on first run' 'Silero VAD 模型未在本地找到 — 首次运行时将自动下载')"
    fi

    # (SenseVoice removed — ASR is WhisperLive-only)

    # GeoIP
    local GEOIP_PATH
    GEOIP_PATH=$(env_get "GEOIP_MMDB_PATH" "")
    if [[ -n "$GEOIP_PATH" && -f "$GEOIP_PATH" ]]; then
        print_ok "$(msg "GeoIP database found" "GeoIP 数据库已找到")"
    elif [[ -n "$GEOIP_PATH" ]]; then
        print_warn "$(msg "GeoIP database not found: ${GEOIP_PATH}" "GeoIP 数据库未找到: ${GEOIP_PATH}")"
        ((WARNINGS++))
    else
        print_info "$(msg 'GeoIP not configured — geolocation disabled' 'GeoIP 未配置 — 地理位置功能不可用')"
    fi

    # ── Build Check ──────────────────────────────────────────────────────
    echo ""
    msg "${BOLD}  Build:${NC}" "${BOLD}  构建:${NC}"

    if [[ -f "${PROJECT_DIR}/target/release/realtime" ]]; then
        local build_time
        build_time=$(stat -f "%Sm" -t "%Y-%m-%d %H:%M" "${PROJECT_DIR}/target/release/realtime" 2>/dev/null || stat -c "%y" "${PROJECT_DIR}/target/release/realtime" 2>/dev/null | cut -d. -f1)
        print_ok "$(msg "Release binary found (built: ${build_time})" "Release 二进制已找到（构建时间: ${build_time}）")"
    else
        print_info "$(msg 'Release binary not found. Run: cargo build --release' 'Release 二进制未找到。运行: cargo build --release')"
    fi

    # ── Summary ──────────────────────────────────────────────────────────
    echo ""
    echo -e "${CYAN}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${NC}"

    if [[ $ERRORS -eq 0 && $WARNINGS -eq 0 ]]; then
        print_ok "$(msg 'All checks passed! Service is ready to start.' '所有检查通过！服务可以正常启动。')"
    elif [[ $ERRORS -eq 0 ]]; then
        msg "  ${YELLOW}${SYM_WARN}${NC} ${WARNINGS} warning(s), 0 errors" \
            "  ${YELLOW}${SYM_WARN}${NC} ${WARNINGS} 个警告, 0 个错误"
        msg "  $(msg 'Service can start, but some features may be unavailable.' '服务可以启动，但部分功能可能不可用。')" \
            "  服务可以启动，但部分功能可能不可用。"
    else
        msg "  ${RED}${SYM_ERR}${NC} ${ERRORS} error(s), ${WARNINGS} warning(s)" \
            "  ${RED}${SYM_ERR}${NC} ${ERRORS} 个错误, ${WARNINGS} 个警告"
        msg "  $(msg 'Please fix errors before starting. Run: ./realtime onboard' '请先修复错误。运行: ./realtime onboard')" \
            "  请先修复错误。运行: ./realtime onboard"
    fi

    echo ""

    # Version check
    check_update 2>/dev/null || true
}
