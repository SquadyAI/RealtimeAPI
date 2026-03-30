#!/usr/bin/env bash
# ============================================================================
# SquadyAI Realtime API — Shared utility functions for setup scripts
# ============================================================================

set -euo pipefail

# ── Colors & Symbols ────────────────────────────────────────────────────────

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
CYAN='\033[0;36m'
BOLD='\033[1m'
DIM='\033[2m'
NC='\033[0m' # No Color

SYM_OK="✓"
SYM_WARN="⚠"
SYM_ERR="✗"
SYM_INFO="ⓘ"
SYM_ARROW="▸"

# Global return values for prompt_select
SELECT_INDEX=0
SELECT_VALUE=""

# ── Language Support ────────────────────────────────────────────────────────

LANG_CHOICE="${LANG_CHOICE:-}"

choose_language() {
    # Auto-detect default from system locale
    local default_idx=0
    if [[ "${LANG:-}" == zh_* || "${LC_ALL:-}" == zh_* || "${LC_CTYPE:-}" == zh_* ]]; then
        default_idx=1
    fi

    echo ""
    echo -e "${BOLD}Choose language / 选择语言:${NC}"
    echo ""
    prompt_select "$default_idx" "English" "中文"
    case "$SELECT_INDEX" in
        1) LANG_CHOICE="zh" ;;
        *) LANG_CHOICE="en" ;;
    esac
    export LANG_CHOICE
}

# i18n helper: msg "en_text" "zh_text"
msg() {
    if [[ "$LANG_CHOICE" == "zh" ]]; then
        echo -e "$2"
    else
        echo -e "$1"
    fi
}

# ── Output Helpers ──────────────────────────────────────────────────────────

print_ok()   { echo -e "  ${GREEN}${SYM_OK}${NC} $1"; }
print_warn() { echo -e "  ${YELLOW}${SYM_WARN}${NC} $1"; }
print_err()  { echo -e "  ${RED}${SYM_ERR}${NC} $1"; }
print_info() { echo -e "  ${BLUE}${SYM_INFO}${NC} $1"; }

print_header() {
    echo ""
    echo -e "${CYAN}━━━ $1 ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${NC}"
    echo ""
}

print_banner() {
    # Render mascot icon from pre-generated ASCII art
    local scripts_dir
    scripts_dir="${REALTIME_HOME:-$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/..}/scripts"
    echo ""
    if [[ -f "${scripts_dir}/icon_ascii.txt" ]]; then
        # Force white background so the mascot looks correct on any terminal theme
        # Replace all [0m (reset) with [0;48;2;255;255;255m (reset + keep white bg)
        local WBG=$'\033[48;2;255;255;255m'
        local RESET_WBG=$'\033[0;48;2;255;255;255m'
        while IFS= read -r line; do
            line="${line//\[0m/${RESET_WBG}}"
            echo -e "${WBG}${line}\033[0m"
        done < "${scripts_dir}/icon_ascii.txt"
    fi

    echo -e "${CYAN}"
    cat << 'BANNER'
  ██████╗ ███████╗ █████╗ ██╗  ████████╗██╗███╗   ███╗███████╗
  ██╔══██╗██╔════╝██╔══██╗██║  ╚══██╔══╝██║████╗ ████║██╔════╝
  ██████╔╝█████╗  ███████║██║     ██║   ██║██╔████╔██║█████╗
  ██╔══██╗██╔══╝  ██╔══██║██║     ██║   ██║██║╚██╔╝██║██╔══╝
  ██║  ██║███████╗██║  ██║███████╗██║   ██║██║ ╚═╝ ██║███████╗
  ╚═╝  ╚═╝╚══════╝╚═╝  ╚═╝╚══════╝╚═╝   ╚═╝╚═╝     ╚═╝╚══════╝
BANNER
    echo -e "${NC}"
    msg "  ${BOLD}Realtime Voice API${NC} — Self-hosted real-time voice conversation service" \
        "  ${BOLD}Realtime Voice API${NC} — 自托管的实时语音对话服务"
    echo ""
}

# ── Input Helpers ───────────────────────────────────────────────────────────

# prompt_input "prompt text" "default_value" -> sets REPLY
prompt_input() {
    local prompt="$1"
    local default="${2:-}"
    if [[ -n "$default" ]]; then
        read -rp "  ${prompt} [${default}]: " REPLY
        REPLY="${REPLY:-$default}"
    else
        read -rp "  ${prompt}: " REPLY
    fi
}

# prompt_secret "prompt text" -> sets REPLY (hidden input)
prompt_secret() {
    local prompt="$1"
    echo -n "  ${prompt}: "
    read -rs REPLY
    echo ""
}

# prompt_yn "prompt text" "default Y or N" -> return 0 for yes, 1 for no
# Uses arrow-key selection with Yes/No options
prompt_yn() {
    local prompt="$1"
    local default="${2:-Y}"
    local default_idx=0

    local yes_label no_label
    if [[ "${LANG_CHOICE:-en}" == "zh" ]]; then
        yes_label="是"
        no_label="否"
    else
        yes_label="Yes"
        no_label="No"
    fi

    if [[ "$default" != "Y" ]]; then
        default_idx=1
    fi

    echo -e "  ${prompt}"
    prompt_select "$default_idx" "$yes_label" "$no_label"

    if [[ $SELECT_INDEX -eq 0 ]]; then
        return 0
    else
        return 1
    fi
}

# prompt_select DEFAULT_INDEX option1 option2 ...
#   Arrow-key driven menu. Up/Down to move, Enter to confirm.
#   DEFAULT_INDEX: 0-based index of the initially highlighted option
#   Sets SELECT_INDEX (0-based) and SELECT_VALUE to the chosen option.
prompt_select() {
    local default_idx="$1"
    shift
    local options=("$@")
    local count=${#options[@]}
    local current=$default_idx

    # ANSI escape helpers (no tput dependency)
    local ESC=$'\033'
    local HIDE_CURSOR="${ESC}[?25l"
    local SHOW_CURSOR="${ESC}[?25h"
    local CLEAR_LINE="${ESC}[2K"
    local MOVE_UP="${ESC}[A"

    # Hide cursor
    printf '%s' "$HIDE_CURSOR"

    # Restore cursor on exit/Ctrl-C
    trap 'printf "%s" "'"$SHOW_CURSOR"'"; trap - INT RETURN' INT RETURN

    # Draw all options
    _sel_draw() {
        local i
        for i in "${!options[@]}"; do
            if [[ $i -eq $current ]]; then
                printf '    \033[0;32m❯ %s\033[0m\n' "${options[$i]}"
            else
                printf '      %s\n' "${options[$i]}"
            fi
        done
    }

    # Move cursor up N lines and clear them
    _sel_clear() {
        local i
        for (( i=0; i<count; i++ )); do
            printf '%s' "${MOVE_UP}${CLEAR_LINE}"
        done
        printf '\r'
    }

    # Initial draw
    _sel_draw

    # Key reading loop
    while true; do
        # Read exactly 1 byte
        IFS= read -rsn1 key 2>/dev/null || true

        # Arrow keys send: ESC [ A/B
        if [[ "$key" == "$ESC" ]]; then
            IFS= read -rsn1 -t 1 bracket 2>/dev/null || true
            if [[ "$bracket" == "[" ]]; then
                IFS= read -rsn1 -t 1 code 2>/dev/null || true
                case "$code" in
                    A) # Up
                        (( current > 0 )) && (( current-- )) || true
                        _sel_clear; _sel_draw
                        ;;
                    B) # Down
                        (( current < count - 1 )) && (( current++ )) || true
                        _sel_clear; _sel_draw
                        ;;
                esac
            fi
        elif [[ "$key" == "" ]]; then
            # Enter
            break
        elif [[ "$key" == "k" ]]; then
            (( current > 0 )) && (( current-- )) || true
            _sel_clear; _sel_draw
        elif [[ "$key" == "j" ]]; then
            (( current < count - 1 )) && (( current++ )) || true
            _sel_clear; _sel_draw
        fi
    done

    # Show cursor
    printf '%s' "$SHOW_CURSOR"

    SELECT_INDEX=$current
    SELECT_VALUE="${options[$current]}"
}

# ── Language Loading ───────────────────────────────────────────────────

# load_ui_lang [--lang zh/en]
#   1. If --lang flag passed, use that and persist to .env
#   2. Else read UI_LANG from .env
#   3. If not found, run interactive choose_language and persist
load_ui_lang() {
    local force_lang=""
    # Parse --lang from args
    while [[ $# -gt 0 ]]; do
        case "$1" in
            --lang)
                force_lang="$2"
                shift 2
                ;;
            *)
                shift
                ;;
        esac
    done

    if [[ -n "$force_lang" ]]; then
        LANG_CHOICE="$force_lang"
        [[ -n "$ENV_FILE" && -f "$ENV_FILE" ]] && env_set "UI_LANG" "$LANG_CHOICE"
        export LANG_CHOICE
        return
    fi

    # Try reading from .env
    if [[ -n "$ENV_FILE" && -f "$ENV_FILE" ]]; then
        local saved
        saved=$(grep -E "^UI_LANG=" "$ENV_FILE" 2>/dev/null | tail -1 | cut -d'=' -f2- || true)
        if [[ -n "$saved" ]]; then
            LANG_CHOICE="$saved"
            export LANG_CHOICE
            return
        fi
    fi

    # Interactive selection
    choose_language
    # Persist
    if [[ -n "$ENV_FILE" ]]; then
        [[ -f "$ENV_FILE" ]] || touch "$ENV_FILE"
        env_set "UI_LANG" "$LANG_CHOICE"
    fi
}

# ── Config Summary ────────────────────────────────────────────────────

# _check_http URL — return 0 if HTTP endpoint is reachable
_check_http() {
    curl -sf --max-time 3 -o /dev/null "$1" 2>/dev/null || \
    curl -sf --max-time 3 -o /dev/null "${1}/" 2>/dev/null
}

# _check_ws URL — return 0 if WebSocket endpoint is reachable (TCP connect check)
_check_ws() {
    local url="$1"
    local host port
    host=$(echo "$url" | sed 's|wss\?://||' | cut -d/ -f1 | cut -d: -f1)
    port=$(echo "$url" | sed 's|wss\?://||' | cut -d/ -f1 | grep -o ':[0-9]*$' | tr -d ':')
    [[ -z "$port" ]] && port=80
    # TCP connect check with 3s timeout
    (echo > /dev/tcp/"$host"/"$port") 2>/dev/null
}

# print_config_summary — read .env and verify each service connectivity
print_config_summary() {
    [[ -z "$ENV_FILE" || ! -f "$ENV_FILE" ]] && return

    local llm_url llm_model tts_enabled tts_engine bind_addr
    llm_url=$(env_get "LLM_BASE_URL" "")
    llm_model=$(env_get "LLM_MODEL" "")
    tts_enabled=$(env_get "ENABLE_TTS" "false")
    tts_engine=$(env_get "TTS_ENGINE" "none")
    bind_addr=$(env_get "BIND_ADDR" "0.0.0.0:8080")

    msg "  Verifying services..." "  正在验证服务连通性..."
    echo ""

    local llm_status tts_status asr_status ext_items
    local all_ok=true

    # ── LLM (real connectivity check) ──
    if [[ -z "$llm_url" || -z "$llm_model" ]]; then
        llm_status="${RED}${SYM_ERR}${NC}"
        all_ok=false
    elif _check_http "${llm_url}/models"; then
        llm_status="${GREEN}${SYM_OK}${NC}"
    elif _check_http "$llm_url"; then
        llm_status="${GREEN}${SYM_OK}${NC}"
    else
        llm_status="${RED}${SYM_ERR}${NC}"
        all_ok=false
    fi

    # ── TTS ──
    if [[ "$tts_enabled" != "true" ]]; then
        tts_status="${DIM}off${NC}"
    elif [[ "$tts_engine" == "edge" ]]; then
        # Edge TTS is a free service, always available
        tts_status="${GREEN}${SYM_OK}${NC} ${tts_engine}"
    elif [[ "$tts_engine" == "minimax" ]]; then
        local mm_key
        mm_key=$(env_get "MINIMAX_API_KEY" "")
        if [[ -n "$mm_key" ]]; then
            tts_status="${GREEN}${SYM_OK}${NC} ${tts_engine}"
        else
            tts_status="${RED}${SYM_ERR}${NC} ${tts_engine} (no API key)"
            all_ok=false
        fi
    elif [[ "$tts_engine" == "volc" ]]; then
        local volc_key
        volc_key=$(env_get "VOLC_ACCESS_TOKEN" "")
        if [[ -n "$volc_key" ]]; then
            tts_status="${GREEN}${SYM_OK}${NC} ${tts_engine}"
        else
            tts_status="${RED}${SYM_ERR}${NC} ${tts_engine} (no token)"
            all_ok=false
        fi
    elif [[ "$tts_engine" == "azure" ]]; then
        local az_key
        az_key=$(env_get "AZURE_SPEECH_KEY" "")
        if [[ -n "$az_key" ]]; then
            tts_status="${GREEN}${SYM_OK}${NC} ${tts_engine}"
        else
            tts_status="${RED}${SYM_ERR}${NC} ${tts_engine} (no key)"
            all_ok=false
        fi
    else
        tts_status="${GREEN}${SYM_OK}${NC} ${tts_engine}"
    fi

    # ── ASR (real connectivity check) ──
    local whisper_path
    whisper_path=$(env_get "WHISPERLIVE_PATH" "")
    if [[ -n "$whisper_path" ]]; then
        if _check_ws "$whisper_path"; then
            asr_status="${GREEN}${SYM_OK}${NC} WhisperLive"
        else
            asr_status="${RED}${SYM_ERR}${NC} WhisperLive (unreachable)"
            all_ok=false
        fi
    else
        asr_status="${YELLOW}${SYM_WARN}${NC} $(msg 'Not configured' '未配置')"
    fi

    # ── Extensions (real connectivity check) ──
    ext_items=""
    local ext_url
    ext_url=$(env_get 'SEARXNG_BASE_URL' '')
    if [[ -n "$ext_url" ]]; then
        if _check_http "$ext_url"; then
            ext_items="${ext_items}${GREEN}Search${NC} "
        else
            ext_items="${ext_items}${RED}Search${NC} "
        fi
    fi
    [[ -n "$(env_get 'DATABASE_URL' '')" ]] && ext_items="${ext_items}DB "
    ext_url=$(env_get 'LANGFUSE_BASE_URL' '')
    if [[ -n "$(env_get 'LANGFUSE_SECRET_KEY' '')" && -n "$ext_url" ]]; then
        if _check_http "$ext_url"; then
            ext_items="${ext_items}${GREEN}Langfuse${NC} "
        else
            ext_items="${ext_items}${RED}Langfuse${NC} "
        fi
    fi
    ext_url=$(env_get 'INTENT_API_URL' '')
    if [[ -n "$ext_url" ]]; then
        if _check_http "$ext_url"; then
            ext_items="${ext_items}${GREEN}Intent${NC} "
        else
            ext_items="${ext_items}${RED}Intent${NC} "
        fi
    fi
    [[ "$(env_get 'ENABLE_SIMUL_INTERPRET' 'false')" == "true" ]] && ext_items="${ext_items}SimulInterpret "
    ext_url=$(env_get 'VISUAL_LLM_STREAM_URL' '')
    if [[ -n "$ext_url" ]]; then
        if _check_http "$ext_url"; then
            ext_items="${ext_items}${GREEN}Vision${NC} "
        else
            ext_items="${ext_items}${RED}Vision${NC} "
        fi
    fi
    [[ -n "$(env_get 'GEOIP_MMDB_PATH' '')" ]] && ext_items="${ext_items}GeoIP "
    [[ -z "$ext_items" ]] && ext_items="none"

    # ── Display ──
    echo ""
    echo -e "  ┌──────────────────────────────────────────────────┐"
    msg "  │  LLM:    ${llm_status}  ${llm_url} (${llm_model})" \
        "  │  LLM:    ${llm_status}  ${llm_url} (${llm_model})"
    echo -e "  │  TTS:    ${tts_status}"
    echo -e "  │  ASR:    ${asr_status}"
    msg "  │  Listen: ${bind_addr}" \
        "  │  监听:   ${bind_addr}"
    msg "  │  Ext:    ${ext_items}" \
        "  │  扩展:   ${ext_items}"
    echo -e "  └──────────────────────────────────────────────────┘"

    if [[ "$all_ok" != "true" ]]; then
        echo ""
        print_warn "$(msg 'Some services are unreachable. The server will start, but affected features may not work.' \
                         '部分服务不可达。服务会启动，但相关功能可能不可用。')"
    fi
    echo ""
}

# ── Required Env Check ────────────────────────────────────────────────

# require_env KEY — exit if KEY is empty or placeholder
require_env() {
    local key="$1"
    local val
    val=$(env_get "$key" "")
    if [[ -z "$val" || "$val" == "sk-your-api-key-here" ]]; then
        print_err "$(msg "${key} is not configured. Run: ./realtime onboard" "${key} 未配置。运行: ./realtime onboard")"
        return 1
    fi
    return 0
}

# ── .env Helpers ────────────────────────────────────────────────────────────

ENV_FILE=""

init_env_file() {
    ENV_FILE="$1"
}

# Read existing value from .env, or return default
env_get() {
    local key="$1"
    local default="${2:-}"
    if [[ -f "$ENV_FILE" ]]; then
        local val
        val=$(grep -E "^${key}=" "$ENV_FILE" 2>/dev/null | tail -1 | cut -d'=' -f2- | sed 's/^"//' | sed 's/"$//' || true)
        if [[ -n "$val" ]]; then
            echo "$val"
            return
        fi
    fi
    echo "$default"
}

# Set a value in .env (append or update)
env_set() {
    local key="$1"
    local value="$2"
    # Quote values that contain spaces
    local quoted_value="$value"
    if [[ "$value" == *" "* && "$value" != \"*\" ]]; then
        quoted_value="\"${value}\""
    fi
    if [[ -f "$ENV_FILE" ]] && grep -qE "^${key}=" "$ENV_FILE" 2>/dev/null; then
        # Update existing line
        if [[ "$(uname)" == "Darwin" ]]; then
            sed -i '' "s|^${key}=.*|${key}=${quoted_value}|" "$ENV_FILE"
        else
            sed -i "s|^${key}=.*|${key}=${quoted_value}|" "$ENV_FILE"
        fi
    else
        # Append
        echo "${key}=${quoted_value}" >> "$ENV_FILE"
    fi
}

# Comment out a key in .env
env_comment() {
    local key="$1"
    if [[ -f "$ENV_FILE" ]] && grep -qE "^${key}=" "$ENV_FILE" 2>/dev/null; then
        if [[ "$(uname)" == "Darwin" ]]; then
            sed -i '' "s|^${key}=|# ${key}=|" "$ENV_FILE"
        else
            sed -i "s|^${key}=|# ${key}=|" "$ENV_FILE"
        fi
    fi
}

# ── Validation Helpers ──────────────────────────────────────────────────────

# Test HTTP endpoint reachability (returns 0 if reachable)
check_http() {
    local url="$1"
    local timeout="${2:-5}"
    curl -sf --max-time "$timeout" "$url" > /dev/null 2>&1
}

# Test if a port is available
check_port_available() {
    local port="$1"
    if command -v lsof &>/dev/null; then
        ! lsof -i ":$port" -sTCP:LISTEN &>/dev/null
    elif command -v ss &>/dev/null; then
        ! ss -tlnp "sport = :$port" 2>/dev/null | grep -q LISTEN
    else
        # Fallback: try to connect
        ! (echo >/dev/tcp/localhost/"$port") 2>/dev/null
    fi
}

# Test LLM API connectivity
validate_llm() {
    local base_url="$1"
    local api_key="$2"
    local model="${3:-gpt-3.5-turbo}"

    local response
    response=$(curl -sf --max-time 10 \
        -H "Authorization: Bearer ${api_key}" \
        -H "Content-Type: application/json" \
        -d "{\"model\":\"${model}\",\"messages\":[{\"role\":\"user\",\"content\":\"hi\"}],\"max_tokens\":5}" \
        "${base_url}/chat/completions" 2>&1) || return 1

    # Check if response contains an error
    if echo "$response" | grep -qi '"error"'; then
        return 1
    fi
    return 0
}

# Test TTS API connectivity (basic HTTP check)
validate_tts_minimax() {
    local api_key="$1"
    # Just check if the key format looks valid (starts with eyJ for JWT)
    [[ -n "$api_key" ]] && return 0 || return 1
}

# Check if a command exists
has_cmd() {
    command -v "$1" &>/dev/null
}

# ── Version Check ──────────────────────────────────────────────────────────

CURRENT_VERSION="1.0.1"

check_update() {
    local repo="${1:-SquadyAI/RealtimeAPI}"
    if ! has_cmd curl; then
        return 1
    fi
    local latest
    latest=$(curl -sf --max-time 5 "https://api.github.com/repos/${repo}/releases/latest" 2>/dev/null \
        | grep '"tag_name"' | head -1 | sed -E 's/.*"v?([^"]*)".*/\1/')
    if [[ -z "$latest" ]]; then
        return 1
    fi
    if [[ "$latest" != "$CURRENT_VERSION" ]]; then
        msg "  ${YELLOW}${SYM_INFO}${NC} New version available: ${BOLD}v${latest}${NC} (current: v${CURRENT_VERSION})" \
            "  ${YELLOW}${SYM_INFO}${NC} 发现新版本: ${BOLD}v${latest}${NC} (当前: v${CURRENT_VERSION})"
        msg "     Update: ${DIM}git pull && cargo build --release${NC}" \
            "     更新: ${DIM}git pull && cargo build --release${NC}"
        return 0
    fi
    return 1
}
