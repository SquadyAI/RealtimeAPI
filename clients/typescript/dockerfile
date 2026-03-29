# 构建阶段
FROM harbor.luxhub.top:2097/docker/node:22 AS builder

WORKDIR /usr/src/app

# 启用 Corepack 并准备 Yarn
RUN corepack enable
RUN corepack prepare yarn@4.9.4 --activate

# 复制依赖文件
COPY package.json yarn.lock ./

# 设置 Yarn 配置
RUN yarn config set nodeLinker node-modules

# 安装依赖
RUN yarn install --frozen-lockfile

# 复制源代码
COPY . .

# 构建项目
RUN yarn build

# 生产阶段 - 使用 nginx 服务静态文件
FROM --platform=linux/amd64 nginx:alpine


# 复制构建产物
COPY --from=builder /usr/src/app/dist /usr/share/nginx/html

# 暴露端口
EXPOSE 80

# 启动 nginx
CMD ["nginx", "-g", "daemon off;"]