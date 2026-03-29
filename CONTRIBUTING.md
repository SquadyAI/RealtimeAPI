# 贡献指南

感谢你对 Realtime API 项目的兴趣！

## 如何贡献

### 报告 Bug

1. 搜索 [Issues](https://github.com/SquadyAI/RealtimeAPI/issues) 确认问题尚未被报告
2. 创建新 Issue，包含：
   - 问题描述
   - 复现步骤
   - 期望行为
   - 实际行为
   - 环境信息（OS、Rust 版本等）

### 提交功能建议

1. 搜索现有 Issues 确认建议尚未存在
2. 创建新 Issue，描述：
   - 功能的使用场景
   - 预期行为
   - 可能的实现方案（可选）

### 提交代码

1. Fork 本仓库
2. 创建功能分支：`git checkout -b feature/your-feature`
3. 提交更改：`git commit -m 'feat: add some feature'`
4. 推送分支：`git push origin feature/your-feature`
5. 创建 Pull Request

## 开发环境

### 前置要求

- Rust nightly（推荐使用 rustup）
- ONNX Runtime 1.22+
- protobuf-compiler

### 本地构建

```bash
# 安装 Rust nightly
rustup default nightly

# 克隆代码
git clone https://github.com/SquadyAI/RealtimeAPI.git
cd RealtimeAPI/server

# 构建
cargo build

# 运行测试
cargo test

# 运行
cp .env.example .env
# 编辑 .env，填写 LLM_API_KEY 和 LLM_BASE_URL
cargo run
```

### 相关项目

- [RealtimeIntent](https://github.com/SquadyAI/RealtimeIntent) — 意图识别服务
- [RealtimeSearch](https://github.com/SquadyAI/RealtimeSearch) — 搜索与翻译 API

## 代码规范

### Rust 代码

- 使用 `cargo fmt` 格式化代码
- 使用 `cargo clippy` 检查代码质量
- 提交前确保 `cargo test` 通过

```bash
# 格式化
cargo fmt

# Lint 检查
cargo clippy -- -D warnings

# 测试
cargo test
```

### Commit 消息规范

使用 [Conventional Commits](https://www.conventionalcommits.org/) 规范：

```
<type>(<scope>): <description>

[optional body]

[optional footer]
```

类型：
- `feat`: 新功能
- `fix`: Bug 修复
- `docs`: 文档更新
- `style`: 代码格式（不影响功能）
- `refactor`: 重构
- `perf`: 性能优化
- `test`: 测试相关
- `chore`: 构建/工具相关

示例：
```
feat(tts): add Azure TTS support
fix(asr): resolve memory leak in session pool
docs: update API documentation
perf(vad): optimize ONNX model inference
```

## Pull Request 要求

- 标题清晰描述更改内容
- 描述中说明更改原因和影响
- 关联相关 Issue（如有）
- 确保 CI 检查通过
- 请求至少一位维护者 Review

## 项目结构

```
Realtime/
├── server/               # Rust 核心服务（WebSocket、ASR/LLM/TTS 管线）
│   ├── src/
│   │   ├── main.rs       # 入口
│   │   ├── rpc/          # WebSocket/HTTP 处理
│   │   ├── asr/          # 语音识别
│   │   ├── llm/          # 大语言模型
│   │   ├── tts/          # 语音合成
│   │   ├── vad/          # 语音活动检测
│   │   ├── agents/       # 代理系统
│   │   ├── audio/        # 音频处理
│   │   └── storage/      # 存储层
│   └── Cargo.toml
└── clients/
    └── typescript/       # TypeScript/React Web Playground
```

## 测试

### 单元测试

```bash
cargo test
```

### 集成测试

```bash
# 启动服务
cargo run --release &

# 运行客户端测试
python realtime_client.py --llm "测试"
```

## 文档

- 公共 API 添加文档注释
- 复杂逻辑添加代码注释
- 更新相关的 markdown 文档

## 行为准则

请友善、尊重地对待他人。我们致力于为所有人提供一个友好、安全的环境。

## 问题？

如有任何问题，欢迎：
- 创建 Issue 讨论
- 加入社区讨论组
