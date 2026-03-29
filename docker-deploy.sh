#!/bin/bash

# ================================
# 实时语音识别系统 Docker 部署脚本
# ================================

set -e

# 颜色输出定义
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# 帮助函数
print_help() {
    echo -e "${BLUE}实时语音识别系统 Docker 部署脚本${NC}"
    echo ""
    echo "用法: $0 [命令] [选项]"
    echo ""
    echo "命令:"
    echo "  build     构建Docker镜像"
    echo "  run       运行容器"
    echo "  dev       启动开发环境"
    echo "  gpu       启动GPU版本"
    echo "  stop      停止所有容器"
    echo "  clean     清理容器和镜像"
    echo "  logs      查看日志"
    echo "  status    查看容器状态"
    echo "  health    检查健康状态"
    echo ""
    echo "选项:"
    echo "  --no-cache    构建时不使用缓存"
    echo "  --pull        构建前拉取最新基础镜像"
    echo "  --gpu         使用GPU支持"
    echo "  --monitoring  启用监控服务"
    echo "  --help        显示此帮助信息"
    echo ""
    echo "示例:"
    echo "  $0 build                    # 构建CPU版本"
    echo "  $0 build --gpu              # 构建CUDA 12.8 GPU版本"
    echo "  $0 run                      # 运行生产环境"
    echo "  $0 dev                      # 运行开发环境"
    echo "  $0 gpu                      # 运行GPU版本"
    echo "  $0 logs realtime-asr        # 查看特定容器日志"
}

# 日志函数
log_info() {
    echo -e "${GREEN}[INFO]${NC} $1"
}

log_warn() {
    echo -e "${YELLOW}[WARN]${NC} $1"
}

log_error() {
    echo -e "${RED}[ERROR]${NC} $1"
}

# 检查Docker和Docker Compose
check_prerequisites() {
    if ! command -v docker &> /dev/null; then
        log_error "Docker未安装，请先安装Docker"
        exit 1
    fi

    if ! command -v docker-compose &> /dev/null; then
        log_error "Docker Compose未安装，请先安装Docker Compose"
        exit 1
    fi

    log_info "Docker环境检查通过"
}

# 检查必要文件
check_files() {
    local required_files=("Dockerfile" "docker-compose.yml" "Cargo.toml")

    for file in "${required_files[@]}"; do
        if [ ! -f "$file" ]; then
            log_error "缺少必要文件: $file"
            exit 1
        fi
    done

    log_info "必要文件检查通过"
}

# 构建镜像
build_image() {
    local build_args=""
    local cache_arg=""
    local pull_arg=""

    # 解析参数
    while [[ $# -gt 0 ]]; do
        case $1 in
            --no-cache)
                cache_arg="--no-cache"
                shift
                ;;
            --pull)
                pull_arg="--pull"
                shift
                ;;
            --gpu)
                build_args="--build-arg ENABLE_CUDA=true --build-arg CUDA_VERSION=12.8"
                log_info "启用CUDA 12.8 GPU支持"
                shift
                ;;
            *)
                shift
                ;;
        esac
    done

    log_info "开始构建Docker镜像..."

    if [ -n "$build_args" ]; then
        log_info "构建CUDA 12.8 GPU版本镜像"
        docker build $cache_arg $pull_arg $build_args -t realtime-asr:gpu .
    else
        log_info "构建CPU版本镜像"
        docker build $cache_arg $pull_arg -t realtime-asr:latest .
    fi

    log_info "镜像构建完成"
}

# 运行生产环境
run_production() {
    log_info "启动生产环境..."
    docker-compose up -d realtime-asr

    # 等待健康检查
    log_info "等待服务启动..."
    sleep 10

    check_health
}

# 运行开发环境
run_development() {
    log_info "启动开发环境..."
    docker-compose --profile dev up realtime-asr-dev
}

# 运行GPU版本
run_gpu() {
    # 检查NVIDIA Docker支持
    if ! command -v nvidia-docker &> /dev/null && ! docker info 2>/dev/null | grep -q nvidia; then
        log_warn "未检测到NVIDIA Docker支持，GPU功能可能无法正常工作"
    fi

    log_info "启动GPU版本..."
    docker-compose --profile gpu up -d realtime-asr-gpu

    # 等待健康检查
    log_info "等待服务启动..."
    sleep 10

    check_health
}

# 停止服务
stop_services() {
    log_info "停止所有服务..."
    docker-compose down
    log_info "服务已停止"
}

# 清理资源
clean_resources() {
    log_warn "这将删除所有容器、镜像和数据卷，确认继续吗? (y/N)"
    read -r response
    if [[ "$response" =~ ^([yY][eE][sS]|[yY])$ ]]; then
        log_info "清理Docker资源..."

        # 停止并删除容器
        docker-compose down -v

        # 删除镜像
        docker rmi realtime-asr:latest realtime-asr:gpu 2>/dev/null || true

        # 清理无用的镜像和容器
        docker system prune -f

        log_info "清理完成"
    else
        log_info "取消清理操作"
    fi
}

# 查看日志
view_logs() {
    local container=${1:-"realtime-asr"}
    log_info "查看 $container 日志..."
    docker-compose logs -f "$container"
}

# 查看状态
view_status() {
    log_info "容器状态:"
    docker-compose ps

    echo ""
    log_info "Docker镜像:"
    docker images | grep realtime-asr || echo "未找到realtime-asr镜像"

    echo ""
    log_info "数据卷:"
    docker volume ls | grep realtime || echo "未找到realtime数据卷"
}

# 健康检查
check_health() {
    local max_attempts=30
    local attempt=1

    log_info "检查服务健康状态..."

    while [ $attempt -le $max_attempts ]; do
        if curl -sf http://localhost:8080/health > /dev/null 2>&1; then
            log_info "✅ 服务健康检查通过"
            log_info "🌐 WebSocket API: ws://localhost:8080"
            log_info "🔊 RTP音频端口: 5004"
            return 0
        fi

        echo -n "."
        sleep 2
        ((attempt++))
    done

    log_error "❌ 服务健康检查失败"
    log_info "查看日志获取更多信息: $0 logs"
    return 1
}

# 主函数
main() {
    case "${1:-help}" in
        build)
            check_prerequisites
            check_files
            shift
            build_image "$@"
            ;;
        run)
            check_prerequisites
            check_files
            run_production
            ;;
        dev)
            check_prerequisites
            check_files
            run_development
            ;;
        gpu)
            check_prerequisites
            check_files
            run_gpu
            ;;
        stop)
            stop_services
            ;;
        clean)
            clean_resources
            ;;
        logs)
            shift
            view_logs "$@"
            ;;
        status)
            view_status
            ;;
        health)
            check_health
            ;;
        help|--help|-h)
            print_help
            ;;
        *)
            log_error "未知命令: $1"
            echo ""
            print_help
            exit 1
            ;;
    esac
}

# 脚本入口
main "$@"
