# =============================================================================
# Docker 构建脚本
# =============================================================================
# 使用方法：修改下面的镜像仓库地址，然后运行此脚本
# =============================================================================

# 固定使用本机持久 builder，并启用 BuildKit 本地缓存
$env:DOCKER_BUILDKIT = '1'
docker buildx inspect be-builder *> $null; if ($LASTEXITCODE -ne 0) { docker buildx create --use --name be-builder | Out-Null } else { docker buildx use be-builder | Out-Null }

# 镜像仓库配置 - 请修改为你的仓库地址
$REGISTRY = "ghcr.io/squadyai"
$IMAGE_NAME = "realtimeapi"
$TAG = "latest"

# 构建 CPU 版（runtime 目标）。--load 适合本机运行/compose 使用。
docker buildx build --file Dockerfile.cpu --target runtime --progress=plain -t "${REGISTRY}/${IMAGE_NAME}:${TAG}" --load .

# 如需 GPU 版（CUDA 12.8），取消下行注释：
# docker buildx build --file Dockerfile.gpu --target runtime --progress=plain -t "${REGISTRY}/${IMAGE_NAME}:${TAG}-gpu" --load .

# 如需直接推送到仓库，将 --load 改为 --push（并确保已 docker login）：
# docker buildx build --file Dockerfile.cpu --target runtime --progress=plain -t "${REGISTRY}/${IMAGE_NAME}:${TAG}" --push .
# docker buildx build --file Dockerfile.gpu --target runtime --progress=plain -t "${REGISTRY}/${IMAGE_NAME}:${TAG}-gpu" --push .

# 如果仍想使用 compose 方式运行/推送，保持不变：
# docker compose up -d
# docker compose push
