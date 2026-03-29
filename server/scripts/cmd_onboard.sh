#!/usr/bin/env bash
# ============================================================================
# SquadyAI Realtime API — Onboard (Setup Wizard)
# Usage: source this from the `realtime` entry script
#   cmd_onboard [--step N] [--force] [--lang zh/en]
# ============================================================================

set -euo pipefail

cmd_onboard() {
    local step_only="" force=false lang_arg=""

    # ── Parse arguments ───────────────────────────────────────────────────
    while [[ $# -gt 0 ]]; do
        case "$1" in
            --step)  step_only="$2"; shift 2 ;;
            --force) force=true; shift ;;
            --lang)  lang_arg="$2"; shift 2 ;;
            *)       shift ;;
        esac
    done

    # ── Language ──────────────────────────────────────────────────────────
    if [[ -n "$lang_arg" ]]; then
        load_ui_lang --lang "$lang_arg"
    else
        # If .env exists, try to load saved language; otherwise interactive
        if [[ -f "$ENV_FILE" ]]; then
            load_ui_lang
        else
            print_banner
            choose_language
        fi
    fi

    # ── Returning user flow ───────────────────────────────────────────────
    if [[ -f "$ENV_FILE" && "$force" == "false" && -z "$step_only" ]]; then
        print_banner
        msg "${BOLD}  Welcome back!${NC}" "${BOLD}  欢迎回来！${NC}"
        print_config_summary

        msg "  What would you like to do?" "  你想做什么？"
        local opts_en=("Start service (./realtime onboard)" "Modify a specific step" "Full reconfigure")
        local opts_zh=("启动服务 (./realtime onboard)" "修改某个步骤" "重新配置全部")
        if [[ "$LANG_CHOICE" == "zh" ]]; then
            prompt_select 0 "${opts_zh[@]}"
        else
            prompt_select 0 "${opts_en[@]}"
        fi

        case "$SELECT_INDEX" in
            0)
                echo ""
                cmd_start
                return $?
                ;;
            1)
                echo ""
                msg "  Which step?" "  选择步骤："
                local steps_en=("Step 1: LLM Configuration" "Step 2: ASR Speech Recognition" "Step 3: TTS Voice Synthesis" "Step 4: Extensions")
                local steps_zh=("Step 1: LLM 配置" "Step 2: ASR 语音识别" "Step 3: TTS 语音合成" "Step 4: 扩展服务")
                if [[ "$LANG_CHOICE" == "zh" ]]; then
                    prompt_select 0 "${steps_zh[@]}"
                else
                    prompt_select 0 "${steps_en[@]}"
                fi
                step_only=$((SELECT_INDEX + 1))
                ;;
            2)
                force=true
                ;;
        esac
    fi

    # ── Fresh install: show banner if not yet shown ───────────────────────
    if [[ ! -f "$ENV_FILE" ]]; then
        print_banner
        if [[ -z "$lang_arg" ]]; then
            # Language was already chosen above for fresh install
            true
        fi
    fi

    # ── Show banner for force/step modes if not yet shown ────────────────
    if [[ -n "$step_only" || "$force" == "true" ]]; then
        print_banner
    fi

    # ── Handle existing .env ──────────────────────────────────────────────
    if [[ -f "$ENV_FILE" && "$force" == "true" ]]; then
        echo ""
        msg "${YELLOW}${SYM_WARN}${NC} Found existing .env file." \
            "${YELLOW}${SYM_WARN}${NC} 检测到已有 .env 配置文件。"
        if prompt_yn "$(msg 'Update existing config?' '更新现有配置？')" "Y"; then
            msg "  Will update existing .env (existing values as defaults)" \
                "  将更新现有 .env（已有值作为默认值）"
        else
            BACKUP="${ENV_FILE}.backup.$(date +%Y%m%d%H%M%S)"
            cp "$ENV_FILE" "$BACKUP"
            msg "  Backed up to ${DIM}${BACKUP}${NC}" \
                "  已备份到 ${DIM}${BACKUP}${NC}"
            : > "$ENV_FILE"
            # Persist language choice in fresh file
            env_set "UI_LANG" "$LANG_CHOICE"
        fi
    elif [[ ! -f "$ENV_FILE" ]]; then
        touch "$ENV_FILE"
        env_set "UI_LANG" "$LANG_CHOICE"
    fi

    # ── Step overview ─────────────────────────────────────────────────────
    if [[ -z "$step_only" ]]; then
        echo ""
        echo -e "${CYAN}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${NC}"
        msg "  The setup wizard will guide you through 4 steps:" \
            "  设置向导将引导你完成以下 4 个步骤："
        echo ""
        msg "  Step 1 ${SYM_ARROW} LLM Configuration ${BOLD}(required)${NC}" \
            "  Step 1 ${SYM_ARROW} LLM 配置 ${BOLD}（必需）${NC}"
        msg "  Step 2 ${SYM_ARROW} ASR Speech Recognition ${BOLD}(required)${NC}" \
            "  Step 2 ${SYM_ARROW} ASR 语音识别 ${BOLD}（必需）${NC}"
        msg "  Step 3 ${SYM_ARROW} TTS Voice Synthesis ${DIM}(optional)${NC}" \
            "  Step 3 ${SYM_ARROW} TTS 语音合成 ${DIM}（可选）${NC}"
        msg "  Step 4 ${SYM_ARROW} Extensions ${DIM}(optional)${NC}" \
            "  Step 4 ${SYM_ARROW} 扩展服务 ${DIM}（可选）${NC}"
        echo -e "${CYAN}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${NC}"
    fi

    # ═══════════════════════════════════════════════════════════════════════
    # Step 1: LLM Configuration (REQUIRED)
    # ═══════════════════════════════════════════════════════════════════════
    if [[ -z "$step_only" || "$step_only" == "1" ]]; then
        _onboard_step_llm
    fi

    # ═══════════════════════════════════════════════════════════════════════
    # Step 2: ASR Configuration (REQUIRED)
    # ═══════════════════════════════════════════════════════════════════════
    if [[ -z "$step_only" || "$step_only" == "2" ]]; then
        _onboard_step_asr
    fi

    # ═══════════════════════════════════════════════════════════════════════
    # Step 3: TTS Configuration (OPTIONAL)
    # ═══════════════════════════════════════════════════════════════════════
    if [[ -z "$step_only" || "$step_only" == "3" ]]; then
        _onboard_step_tts
    fi

    # ═══════════════════════════════════════════════════════════════════════
    # Step 4: Extensions (OPTIONAL)
    # ═══════════════════════════════════════════════════════════════════════
    if [[ -z "$step_only" || "$step_only" == "4" ]]; then
        _onboard_step_extensions
    fi

    # ═══════════════════════════════════════════════════════════════════════
    # Summary
    # ═══════════════════════════════════════════════════════════════════════
    echo ""
    echo -e "${CYAN}━━━ $(msg 'Setup Complete!' '设置完成！') ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${NC}"
    echo ""
    msg "  Generated config: ${BOLD}.env${NC}" \
        "  已生成配置文件: ${BOLD}.env${NC}"

    print_config_summary

    # Auto-start after setup
    cmd_start

    # Version check
    check_update 2>/dev/null || true
}

# ── Step 1: LLM ──────────────────────────────────────────────────────────

_onboard_step_llm() {
    print_header "$(msg 'Step 1/4: LLM Configuration' 'Step 1/4: LLM 配置')"

    msg "  You need an OpenAI-compatible LLM API." \
        "  你需要一个 OpenAI 兼容的 LLM API。"
    echo ""

    # Provider selection
    local LLM_PROVIDERS_EN=("Groq (free, fastest — console.groq.com)" "OpenAI (api.openai.com)" "DeepSeek (api.deepseek.com)" "SiliconFlow (api.siliconflow.cn)" "Ollama local (localhost:11434)" "Custom OpenAI-compatible API")
    local LLM_PROVIDERS_ZH=("Groq（免费，最快 — console.groq.com）" "OpenAI (api.openai.com)" "DeepSeek (api.deepseek.com)" "SiliconFlow 硅基流动 (api.siliconflow.cn)" "Ollama 本地模型 (localhost:11434)" "自定义 OpenAI 兼容 API")
    local LLM_URLS=("https://api.groq.com/openai/v1" "https://api.openai.com/v1" "https://api.deepseek.com/v1" "https://api.siliconflow.cn/v1" "http://localhost:11434/v1" "")
    local LLM_MODELS=("llama-3.3-70b-versatile" "gpt-4o-mini" "deepseek-chat" "Qwen/Qwen3-8B" "llama3.2" "")
    local PROVIDERS

    if [[ "$LANG_CHOICE" == "zh" ]]; then
        PROVIDERS=("${LLM_PROVIDERS_ZH[@]}")
    else
        PROVIDERS=("${LLM_PROVIDERS_EN[@]}")
    fi

    # Detect existing provider for default selection
    local existing_url default_provider=0 matched=false
    existing_url=$(env_get "LLM_BASE_URL" "")
    if [[ -n "$existing_url" ]]; then
        for i in "${!LLM_URLS[@]}"; do
            if [[ -n "${LLM_URLS[$i]}" && "$existing_url" == "${LLM_URLS[$i]}" ]]; then
                default_provider=$i
                matched=true
                break
            fi
        done
        # If URL doesn't match any preset, default to "Custom"
        if [[ "$matched" == "false" ]]; then
            default_provider=4
        fi
    fi

    msg "  ? $(msg 'Choose LLM provider:' '选择 LLM 提供商：')" \
        "  ? 选择 LLM 提供商："
    prompt_select "$default_provider" "${PROVIDERS[@]}"
    local provider_idx=$SELECT_INDEX

    local LLM_BASE_URL="${LLM_URLS[$provider_idx]}"
    local LLM_MODEL_DEFAULT="${LLM_MODELS[$provider_idx]}"

    # Special handling for Ollama
    if [[ $provider_idx -eq 4 ]]; then
        echo ""
        msg "  Checking Ollama..." "  正在检测 Ollama..."
        if check_http "http://localhost:11434/api/tags" 3; then
            print_ok "$(msg 'Ollama is running!' 'Ollama 运行中！')"
            local models
            models=$(curl -sf --max-time 5 "http://localhost:11434/api/tags" 2>/dev/null | grep -o '"name":"[^"]*"' | sed 's/"name":"//;s/"//' | head -5)
            if [[ -n "$models" ]]; then
                msg "  Available models:" "  可用模型："
                echo "$models" | while read -r m; do echo "    - $m"; done
            fi
        else
            print_warn "$(msg 'Ollama not detected at localhost:11434. Make sure it is running before starting the service.' 'localhost:11434 未检测到 Ollama。请确保启动服务前 Ollama 已运行。')"
        fi
    fi

    # Custom URL
    if [[ $provider_idx -eq 5 ]]; then
        echo ""
        existing_url=$(env_get "LLM_BASE_URL" "https://api.example.com/v1")
        prompt_input "$(msg 'API Base URL' 'API 地址')" "$existing_url"
        LLM_BASE_URL="$REPLY"
    fi

    # API Key (not needed for Ollama or self-hosted LLMs)
    local LLM_API_KEY
    if [[ $provider_idx -eq 4 ]]; then
        LLM_API_KEY="ollama"
    else
        echo ""
        local existing_key
        existing_key=$(env_get "LLM_API_KEY" "")
        if [[ -n "$existing_key" && "$existing_key" != "sk-your-api-key-here" ]]; then
            local masked="${existing_key:0:8}...${existing_key: -4}"
            msg "  Existing API Key: ${DIM}${masked}${NC}" \
                "  已有 API Key: ${DIM}${masked}${NC}"
            if prompt_yn "$(msg 'Keep existing key?' '保留现有 Key？')" "Y"; then
                LLM_API_KEY="$existing_key"
            else
                prompt_secret "$(msg 'API Key' 'API Key')"
                LLM_API_KEY="$REPLY"
            fi
        else
            msg "  ${DIM}$(msg 'Press Enter to skip if your LLM does not require a key.' '如果你的 LLM 不需要 Key，直接按回车跳过。')${NC}" ""
            prompt_input "$(msg 'API Key' 'API Key')" ""
            LLM_API_KEY="$REPLY"
        fi
    fi

    # Model name
    echo ""
    local existing_model LLM_MODEL
    existing_model=$(env_get "LLM_MODEL" "$LLM_MODEL_DEFAULT")
    prompt_input "$(msg 'Model name' '模型名称')" "$existing_model"
    LLM_MODEL="$REPLY"

    # Validate connection
    echo ""
    msg "  Validating LLM connection..." "  正在验证 LLM 连接..."

    if validate_llm "$LLM_BASE_URL" "$LLM_API_KEY" "$LLM_MODEL"; then
        print_ok "$(msg 'LLM API connection successful!' 'LLM API 连接成功！')"
    else
        print_warn "$(msg 'Could not validate LLM API. The config has been saved — please verify your API key and URL.' '无法验证 LLM API。配置已保存，请检查 API Key 和地址是否正确。')"
    fi

    # System prompt
    echo ""
    local DEFAULT_PROMPT="You are a helpful voice assistant. Keep responses concise and conversational. Reply in the same language as the user."
    local SYSTEM_PROMPT="$DEFAULT_PROMPT"
    if prompt_yn "$(msg 'Customize system prompt?' '自定义系统提示词？')" "n"; then
        local existing_prompt
        existing_prompt=$(env_get "DEFAULT_SYSTEM_PROMPT" "$DEFAULT_PROMPT")
        prompt_input "$(msg 'System prompt' '系统提示词')" "$existing_prompt"
        SYSTEM_PROMPT="$REPLY"
    fi

    # Write LLM config
    env_set "LLM_BASE_URL" "$LLM_BASE_URL"
    env_set "LLM_API_KEY" "$LLM_API_KEY"
    env_set "LLM_MODEL" "$LLM_MODEL"
    env_set "LLM_TIMEOUT_SECS" "$(env_get 'LLM_TIMEOUT_SECS' '30')"
    env_set "DEFAULT_SYSTEM_PROMPT" "$SYSTEM_PROMPT"

    print_ok "$(msg 'LLM configuration saved!' 'LLM 配置已保存！')"
}

# ── Step 2: ASR (REQUIRED) ────────────────────────────────────────────────

_onboard_step_asr() {
    print_header "$(msg 'Step 2/4: ASR Speech Recognition' 'Step 2/4: ASR 语音识别')"

    msg "  ASR converts speech to text. This project uses WhisperLive (open-source)." \
        "  ASR 将语音转为文字。本项目使用 WhisperLive（开源）。"
    echo ""
    msg "  ${DIM}$(msg 'You need a running WhisperLive server. Deploy with Docker:' '你需要一个运行中的 WhisperLive 服务，一行命令部署：')${NC}" ""
    msg "  ${BOLD}docker run -d -p 9090:9090 ghcr.io/collabora/whisperlive:latest${NC}" \
        "  ${BOLD}docker run -d -p 9090:9090 ghcr.io/collabora/whisperlive:latest${NC}"
    msg "  ${DIM}$(msg 'GitHub: https://github.com/collabora/WhisperLive' 'GitHub: https://github.com/collabora/WhisperLive')${NC}" ""
    echo ""

    local existing_whisper whisper_url
    existing_whisper=$(env_get "WHISPERLIVE_PATH" "ws://localhost:9090")
    prompt_input "WhisperLive WebSocket URL" "$existing_whisper"
    whisper_url="$REPLY"
    env_set "WHISPERLIVE_PATH" "$whisper_url"

    # Validate connection
    msg "  $(msg 'Validating WhisperLive connection...' '正在验证 WhisperLive 连接...')" ""
    if check_ws "$whisper_url" 3 2>/dev/null || check_http "${whisper_url%ws://*}http://${whisper_url#ws://}" 3 2>/dev/null; then
        print_ok "$(msg "ASR: WhisperLive (${whisper_url})" "ASR: WhisperLive (${whisper_url})")"
    else
        print_warn "$(msg "WhisperLive not reachable at ${whisper_url}. Make sure it is running before starting." "WhisperLive 无法连接 ${whisper_url}。请确保启动服务前已运行。")"
    fi

    # VAD defaults
    env_set "VAD_THRESHOLD" "$(env_get 'VAD_THRESHOLD' '0.6')"
    env_set "VAD_MIN_SILENCE_MS" "$(env_get 'VAD_MIN_SILENCE_MS' '500')"
    env_set "VAD_MIN_SPEECH_MS" "$(env_get 'VAD_MIN_SPEECH_MS' '120')"
    env_set "SAMPLE_RATE" "$(env_get 'SAMPLE_RATE' '16000')"
    env_set "CHUNK_SIZE" "$(env_get 'CHUNK_SIZE' '1024')"
    env_set "BUFFER_SIZE" "$(env_get 'BUFFER_SIZE' '8192')"

    # Deployment defaults
    env_set "BIND_ADDR" "$(env_get 'BIND_ADDR' '0.0.0.0:8080')"
    env_set "MAX_CONCURRENT_SESSIONS" "$(env_get 'MAX_CONCURRENT_SESSIONS' '100')"
    env_set "RUST_LOG" "$(env_get 'RUST_LOG' 'info')"
    env_set "RUST_BACKTRACE" "$(env_get 'RUST_BACKTRACE' '1')"

    print_ok "$(msg 'ASR configuration saved!' 'ASR 配置已保存！')"
}

# ── Step 3: TTS ──────────────────────────────────────────────────────────

_onboard_step_tts() {
    print_header "$(msg 'Step 3/4: TTS Voice Synthesis' 'Step 3/4: TTS 语音合成')"

    msg "  TTS enables the AI to \"speak\". Without it, only text responses are returned." \
        "  TTS 让 AI 可以「说话」。不配置则只返回文本。"
    echo ""

    if prompt_yn "$(msg 'Enable TTS voice synthesis?' '启用 TTS 语音合成？')" "Y"; then
        echo ""

        local TTS_ENGINES_EN=("Edge TTS (free, 100+ languages, recommended)" "Azure Speech (high quality, requires key)" "MiniMax (Chinese optimized, 50+ voices)" "VolcEngine (alternative)" "Baidu TTS")
        local TTS_ENGINES_ZH=("Edge TTS（免费，100+ 语言，推荐入门）" "Azure 语音（高质量，需要 Key）" "MiniMax（中文优化，50+ 音色）" "火山引擎 VolcEngine（备选）" "百度 TTS")
        local TTS_ENGINE_IDS=("edge" "azure" "minimax" "volc" "baidu")
        local ENGINES

        if [[ "$LANG_CHOICE" == "zh" ]]; then
            ENGINES=("${TTS_ENGINES_ZH[@]}")
        else
            ENGINES=("${TTS_ENGINES_EN[@]}")
        fi

        # Detect existing engine for default
        local existing_engine default_tts=0
        existing_engine=$(env_get "TTS_ENGINE" "edge")
        for i in "${!TTS_ENGINE_IDS[@]}"; do
            if [[ "${TTS_ENGINE_IDS[$i]}" == "$existing_engine" ]]; then
                default_tts=$i
                break
            fi
        done

        msg "  ? $(msg 'Choose TTS engine:' '选择 TTS 引擎：')" \
            "  ? 选择 TTS 引擎："
        prompt_select "$default_tts" "${ENGINES[@]}"
        local tts_idx=$SELECT_INDEX
        local TTS_ENGINE="${TTS_ENGINE_IDS[$tts_idx]}"

        case "$TTS_ENGINE" in
            edge)
                echo ""
                print_ok "$(msg 'Edge TTS requires no additional configuration. Enabled!' 'Edge TTS 无需额外配置，已启用！')"
                ;;
            azure)
                echo ""
                local existing_key AZURE_KEY existing_region AZURE_REGION
                existing_key=$(env_get "AZURE_SPEECH_KEY" "")
                if [[ -n "$existing_key" ]]; then
                    local masked="${existing_key:0:8}...${existing_key: -4}"
                    msg "  Existing key: ${DIM}${masked}${NC}" "  已有 Key: ${DIM}${masked}${NC}"
                    if prompt_yn "$(msg 'Keep existing key?' '保留现有 Key？')" "Y"; then
                        AZURE_KEY="$existing_key"
                    else
                        prompt_secret "Azure Speech Key"
                        AZURE_KEY="$REPLY"
                    fi
                else
                    prompt_secret "Azure Speech Key"
                    AZURE_KEY="$REPLY"
                fi
                existing_region=$(env_get "AZURE_SPEECH_REGION" "eastus")
                prompt_input "Azure Region" "$existing_region"
                AZURE_REGION="$REPLY"

                env_set "AZURE_SPEECH_KEY" "$AZURE_KEY"
                env_set "AZURE_SPEECH_REGION" "$AZURE_REGION"
                print_ok "$(msg 'Azure Speech configured!' 'Azure 语音已配置！')"
                ;;
            minimax)
                echo ""
                local existing_key MINIMAX_KEY
                existing_key=$(env_get "MINIMAX_API_KEY" "")
                if [[ -n "$existing_key" && "$existing_key" != "your_minimax_api_key_here" ]]; then
                    local masked="${existing_key:0:8}..."
                    msg "  Existing key: ${DIM}${masked}${NC}" "  已有 Key: ${DIM}${masked}${NC}"
                    if prompt_yn "$(msg 'Keep existing key?' '保留现有 Key？')" "Y"; then
                        MINIMAX_KEY="$existing_key"
                    else
                        prompt_secret "MiniMax API Key"
                        MINIMAX_KEY="$REPLY"
                    fi
                else
                    prompt_secret "MiniMax API Key"
                    MINIMAX_KEY="$REPLY"
                fi
                env_set "MINIMAX_API_KEY" "$MINIMAX_KEY"
                print_ok "$(msg 'MiniMax TTS configured!' 'MiniMax TTS 已配置！')"
                ;;
            volc)
                echo ""
                local existing_app VOLC_APP_ID existing_token VOLC_TOKEN existing_speaker VOLC_SPEAKER
                existing_app=$(env_get "VOLC_APP_ID" "")
                prompt_input "VolcEngine App ID" "$existing_app"
                VOLC_APP_ID="$REPLY"

                existing_token=$(env_get "VOLC_ACCESS_TOKEN" "")
                if [[ -n "$existing_token" ]]; then
                    local masked="${existing_token:0:8}..."
                    msg "  Existing token: ${DIM}${masked}${NC}" "  已有 Token: ${DIM}${masked}${NC}"
                    if prompt_yn "$(msg 'Keep existing token?' '保留现有 Token？')" "Y"; then
                        VOLC_TOKEN="$existing_token"
                    else
                        prompt_secret "VolcEngine Access Token"
                        VOLC_TOKEN="$REPLY"
                    fi
                else
                    prompt_secret "VolcEngine Access Token"
                    VOLC_TOKEN="$REPLY"
                fi

                existing_speaker=$(env_get "VOLC_SPEAKER" "zh_female_wanwanxiaohe_moon_bigtts")
                prompt_input "$(msg 'Speaker voice ID' '发音人 ID')" "$existing_speaker"
                VOLC_SPEAKER="$REPLY"

                env_set "VOLC_APP_ID" "$VOLC_APP_ID"
                env_set "VOLC_ACCESS_TOKEN" "$VOLC_TOKEN"
                env_set "VOLC_RESOURCE_ID" "$(env_get 'VOLC_RESOURCE_ID' 'volc.service_type.10029')"
                env_set "VOLC_SPEAKER" "$VOLC_SPEAKER"
                print_ok "$(msg 'VolcEngine TTS configured!' '火山引擎 TTS 已配置！')"
                ;;
            baidu)
                echo ""
                local existing_key BAIDU_KEY existing_secret BAIDU_SECRET existing_per BAIDU_PER
                existing_key=$(env_get "BAIDU_TTS_API_KEY" "")
                if [[ -n "$existing_key" ]]; then
                    local masked="${existing_key:0:8}..."
                    msg "  Existing key: ${DIM}${masked}${NC}" "  已有 Key: ${DIM}${masked}${NC}"
                    if prompt_yn "$(msg 'Keep existing key?' '保留现有 Key？')" "Y"; then
                        BAIDU_KEY="$existing_key"
                    else
                        prompt_secret "Baidu TTS API Key"
                        BAIDU_KEY="$REPLY"
                    fi
                else
                    prompt_secret "Baidu TTS API Key"
                    BAIDU_KEY="$REPLY"
                fi

                existing_secret=$(env_get "BAIDU_TTS_SECRET_KEY" "")
                if [[ -n "$existing_secret" ]]; then
                    if prompt_yn "$(msg 'Keep existing Secret Key?' '保留现有 Secret Key？')" "Y"; then
                        BAIDU_SECRET="$existing_secret"
                    else
                        prompt_secret "Baidu TTS Secret Key"
                        BAIDU_SECRET="$REPLY"
                    fi
                else
                    prompt_secret "Baidu TTS Secret Key"
                    BAIDU_SECRET="$REPLY"
                fi

                existing_per=$(env_get "BAIDU_TTS_PER" "4197")
                prompt_input "$(msg 'Speaker ID' '发音人 ID')" "$existing_per"
                BAIDU_PER="$REPLY"

                env_set "BAIDU_TTS_API_KEY" "$BAIDU_KEY"
                env_set "BAIDU_TTS_SECRET_KEY" "$BAIDU_SECRET"
                env_set "BAIDU_TTS_PER" "$BAIDU_PER"
                print_ok "$(msg 'Baidu TTS configured!' '百度 TTS 已配置！')"
                ;;
        esac

        env_set "ENABLE_TTS" "true"
        env_set "TTS_ENGINE" "$TTS_ENGINE"
        env_set "TTS_MAX_CONCURRENT" "$(env_get 'TTS_MAX_CONCURRENT' '5')"
        env_set "TTS_BUFFER_SIZE" "$(env_get 'TTS_BUFFER_SIZE' '8192')"
    else
        env_set "ENABLE_TTS" "false"
        print_info "$(msg 'TTS disabled. Text-only mode.' 'TTS 已禁用。纯文本模式。')"
    fi
}

# ── Step 4: Extensions ────────────────────────────────────────────────────

_onboard_step_extensions() {
    print_header "$(msg 'Step 4/4: Extensions' 'Step 4/4: 扩展服务')"

    msg "  These optional services enhance functionality." \
        "  以下可选服务可以增强功能。"
    echo ""

    # ── Default-on: Intent Recognition ──
    echo ""
    msg "  ${BOLD}$(msg 'Default capabilities:' '默认能力：')${NC}" ""
    echo ""

    local existing_intent
    existing_intent=$(env_get "INTENT_API_URL" "")
    if prompt_yn "$(msg 'Enable intent recognition?' '启用意图识别？')" "n"; then
        local intent_url
        existing_intent=$(env_get "INTENT_API_URL" "http://localhost:8000/intent")
        prompt_input "Intent API URL" "$existing_intent"
        intent_url="$REPLY"
        env_set "INTENT_API_URL" "$intent_url"
        if check_http "$intent_url" 3; then
            print_ok "$(msg "Intent API reachable (${intent_url})" "意图识别 API 可达 (${intent_url})")"
        else
            print_warn "$(msg "Intent API not reachable at ${intent_url}" "意图识别 API 无法连接 ${intent_url}")"
        fi
    else
        env_comment "INTENT_API_URL"
        msg "  ${DIM}SquadyAI repo: https://github.com/SquadyAI/RealtimeIntent${NC}" \
            "  ${DIM}SquadyAI repo: https://github.com/SquadyAI/RealtimeIntent${NC}"
    fi

    echo ""

    # ── Default-on: SimulInterpret ──
    local existing_simul
    existing_simul=$(env_get "ENABLE_SIMUL_INTERPRET" "false")
    if prompt_yn "$(msg 'Enable simultaneous interpretation?' '启用同声传译？')" "$([ "$existing_simul" == "true" ] && echo Y || echo n)"; then
        env_set "ENABLE_SIMUL_INTERPRET" "true"
        print_ok "$(msg 'Simultaneous interpretation enabled.' '同声传译已启用。')"
    else
        env_set "ENABLE_SIMUL_INTERPRET" "false"
        print_info "$(msg 'Simultaneous interpretation disabled.' '同声传译已禁用。')"
    fi

    echo ""

    # ── Built-in tools (user selects) ──
    msg "  ${BOLD}$(msg 'Built-in tools:' '内置工具：')${NC}" ""
    echo ""

    # SearXNG Search
    local existing_searx
    existing_searx=$(env_get "SEARXNG_BASE_URL" "")
    if prompt_yn "$(msg 'Enable search engine (SearXNG)?' '启用搜索引擎（SearXNG）？')" "n"; then
        existing_searx=$(env_get "SEARXNG_BASE_URL" "http://localhost:8787")
        prompt_input "SearXNG URL" "$existing_searx"
        env_set "SEARXNG_BASE_URL" "$REPLY"
        env_set "SEARXNG_TIMEOUT_MS" "$(env_get 'SEARXNG_TIMEOUT_MS' '5000')"
        env_set "SEARXNG_MAX_RESULTS" "$(env_get 'SEARXNG_MAX_RESULTS' '20')"

        if check_http "$REPLY/healthz" 3 || check_http "$REPLY" 3; then
            print_ok "$(msg 'SearXNG is reachable!' 'SearXNG 连接成功！')"
        else
            print_warn "$(msg 'SearXNG not reachable. Make sure it is running before using search.' 'SearXNG 无法连接。使用搜索功能前请确保其已运行。')"
        fi
    else
        env_comment "SEARXNG_BASE_URL"
        msg "  ${DIM}SquadyAI repo: https://github.com/SquadyAI/RealtimeSearch${NC}" \
            "  ${DIM}SquadyAI repo: https://github.com/SquadyAI/RealtimeSearch${NC}"
    fi

}
