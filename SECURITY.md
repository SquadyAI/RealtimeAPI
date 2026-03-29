# 安全政策

## 支持的版本

| 版本 | 支持状态 |
|------|----------|
| 1.x  | 支持 |
| < 1.0| 不支持 |

## 报告漏洞

如果你发现安全漏洞，请**不要**创建公开的 Issue。

请通过以下方式私下报告：

1. 发送邮件至：tiantian.zhan@aya.yale.edu
2. 或使用 GitHub 的 [私密漏洞报告](https://docs.github.com/en/code-security/security-advisories/guidance-on-reporting-and-writing/privately-reporting-a-security-vulnerability) 功能

### 报告内容

请在报告中包含：

- 漏洞描述
- 复现步骤
- 影响范围
- 可能的修复建议（如有）

### 响应时间

- 我们会在 **48 小时内** 确认收到报告
- 我们会在 **7 天内** 提供初步评估
- 修复时间取决于漏洞严重程度

## 安全最佳实践

使用本项目时，请注意：

### 1. 不要提交敏感信息

API 密钥、密码等不应提交到代码库：

```bash
# 错误示例
LLM_API_KEY=sk-xxx  # 不要硬编码

# 正确做法
LLM_API_KEY=${LLM_API_KEY}  # 使用环境变量
```

### 2. 使用环境变量

所有配置应通过环境变量传入：

```bash
# 使用 .env 文件
cd server
cp .env.example .env
# 编辑 .env（不要提交）
```

### 3. 网络隔离

生产环境建议：

- LLM/TTS 服务部署在内网
- WebSocket 端口使用反向代理
- 启用 TLS/HTTPS

### 4. 定期更新依赖

```bash
# 检查安全漏洞
cargo audit

# 更新依赖
cargo update
```

### 5. 日志安全

- 不要在日志中记录敏感信息
- 生产环境使用 `RUST_LOG=warn` 或更低级别

## 已知安全考虑

### WebSocket 连接

- 默认不启用认证，适用于内网部署
- 生产环境建议添加 JWT 认证
- 考虑配置连接速率限制

### 音频数据

- 用户音频默认存储在内存
- 启用 PostgreSQL 持久化时注意数据加密
- 定期清理过期会话数据

### Function Calling

- 工具调用默认受限于预定义工具集
- 自定义工具需注意输入验证
- MCP 服务器应配置在可信网络
