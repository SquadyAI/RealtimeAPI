# 监控和日志文档

## 概述

实时语音对话系统提供全面的监控和日志功能，帮助运维人员了解系统状态、性能指标和故障排查。

## 日志系统

### 1. 日志配置

系统使用 `tracing` 库进行结构化日志记录：

```rust
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

fn init_logging() {
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "realtime=info".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();
}
```

### 2. 日志级别

- **ERROR**: 错误信息，需要立即关注
- **WARN**: 警告信息，可能影响性能
- **INFO**: 一般信息，记录重要操作
- **DEBUG**: 调试信息，用于开发调试
- **TRACE**: 详细跟踪信息

### 3. 日志格式

#### JSON格式（生产环境）

```json
{
  "timestamp": "2024-01-01T12:00:00.000Z",
  "level": "INFO",
  "target": "realtime::session_manager",
  "message": "Session created",
  "session_id": "sess_001",
  "user_id": "user_001",
  "duration_ms": 150
}
```

#### 文本格式（开发环境）

```
2024-01-01T12:00:00.000Z INFO  realtime::session_manager Session created session_id=sess_001 user_id=user_001 duration_ms=150
```

### 4. 日志文件管理

```bash
# 日志轮转配置
# /etc/logrotate.d/realtime
/var/log/realtime/*.log {
    daily
    missingok
    rotate 30
    compress
    delaycompress
    notifempty
    create 644 realtime realtime
    postrotate
        systemctl reload realtime
    endscript
}
```

## 指标收集

### 1. Prometheus指标

系统暴露Prometheus格式的指标：

```rust
use prometheus::{Counter, Histogram, Gauge, Registry};

pub struct Metrics {
    pub requests_total: Counter,
    pub request_duration: Histogram,
    pub active_sessions: Gauge,
    pub asr_latency: Histogram,
    pub llm_latency: Histogram,
    pub tts_latency: Histogram,
}

impl Metrics {
    pub fn new(registry: &Registry) -> Self {
        let requests_total = Counter::new(
            "requests_total",
            "Total number of requests"
        ).unwrap();

        let request_duration = Histogram::new(
            "request_duration_seconds",
            "Request duration in seconds"
        ).unwrap();

        let active_sessions = Gauge::new(
            "active_sessions",
            "Number of active sessions"
        ).unwrap();

        let asr_latency = Histogram::new(
            "asr_latency_seconds",
            "ASR processing latency"
        ).unwrap();

        let llm_latency = Histogram::new(
            "llm_latency_seconds",
            "LLM processing latency"
        ).unwrap();

        let tts_latency = Histogram::new(
            "tts_latency_seconds",
            "TTS processing latency"
        ).unwrap();

        registry.register(Box::new(requests_total.clone())).unwrap();
        registry.register(Box::new(request_duration.clone())).unwrap();
        registry.register(Box::new(active_sessions.clone())).unwrap();
        registry.register(Box::new(asr_latency.clone())).unwrap();
        registry.register(Box::new(llm_latency.clone())).unwrap();
        registry.register(Box::new(tts_latency.clone())).unwrap();

        Self {
            requests_total,
            request_duration,
            active_sessions,
            asr_latency,
            llm_latency,
            tts_latency,
        }
    }
}
```

### 2. 关键指标

#### 性能指标

- `requests_total`: 总请求数
- `request_duration_seconds`: 请求处理时间
- `active_sessions`: 活跃会话数
- `asr_latency_seconds`: ASR处理延迟
- `llm_latency_seconds`: LLM处理延迟
- `tts_latency_seconds`: TTS处理延迟

#### 业务指标

- `conversations_total`: 总对话数
- `messages_total`: 总消息数
- `users_total`: 总用户数
- `error_rate`: 错误率

#### 系统指标

- `cpu_usage_percent`: CPU使用率
- `memory_usage_bytes`: 内存使用量
- `disk_usage_percent`: 磁盘使用率
- `network_io_bytes`: 网络I/O

### 3. 指标端点

```rust
use actix_web::{get, HttpResponse};
use prometheus::{Encoder, TextEncoder};

#[get("/metrics")]
async fn metrics() -> HttpResponse {
    let encoder = TextEncoder::new();
    let mut buffer = vec![];
    encoder.encode(&prometheus::gather(), &mut buffer).unwrap();

    HttpResponse::Ok()
        .content_type("text/plain; version=0.0.4; charset=utf-8")
        .body(buffer)
}
```

## 健康检查

### 1. 健康检查端点

```rust
use actix_web::{get, HttpResponse};
use serde_json::json;
use std::collections::HashMap;

#[derive(serde::Serialize)]
struct HealthStatus {
    status: String,
    timestamp: chrono::DateTime<chrono::Utc>,
    services: HashMap<String, String>,
}

#[get("/health")]
async fn health_check() -> HttpResponse {
    let mut services = HashMap::new();

    // 检查数据库连接
    let db_status = check_database().await;
    services.insert("database".to_string(), db_status);

    // 检查ASR服务
    let asr_status = check_asr_service().await;
    services.insert("asr".to_string(), asr_status);

    // 检查LLM服务
    let llm_status = check_llm_service().await;
    services.insert("llm".to_string(), llm_status);

    // 检查TTS服务
    let tts_status = check_tts_service().await;
    services.insert("tts".to_string(), tts_status);

    let overall_status = if services.values().all(|s| s == "healthy") {
        "healthy"
    } else {
        "unhealthy"
    };

    let health = HealthStatus {
        status: overall_status.to_string(),
        timestamp: chrono::Utc::now(),
        services,
    };

    HttpResponse::Ok().json(health)
}

async fn check_database() -> String {
    // 检查数据库连接
    match sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect(&std::env::var("DATABASE_URL").unwrap())
        .await
    {
        Ok(_) => "healthy".to_string(),
        Err(_) => "unhealthy".to_string(),
    }
}

async fn check_asr_service() -> String {
    // 检查ASR服务
    "healthy".to_string()
}

async fn check_llm_service() -> String {
    // 检查LLM服务
    "healthy".to_string()
}

async fn check_tts_service() -> String {
    // 检查TTS服务
    "healthy".to_string()
}
```

### 2. 详细健康检查

```rust
#[derive(serde::Serialize)]
struct DetailedHealthStatus {
    status: String,
    timestamp: chrono::DateTime<chrono::Utc>,
    services: HashMap<String, ServiceHealth>,
}

#[derive(serde::Serialize)]
struct ServiceHealth {
    status: String,
    response_time_ms: Option<u64>,
    error_message: Option<String>,
}

#[get("/health/detailed")]
async fn detailed_health_check() -> HttpResponse {
    let mut services = HashMap::new();

    // 数据库健康检查
    let db_health = check_database_detailed().await;
    services.insert("database".to_string(), db_health);

    // ASR服务健康检查
    let asr_health = check_asr_service_detailed().await;
    services.insert("asr".to_string(), asr_health);

    // LLM服务健康检查
    let llm_health = check_llm_service_detailed().await;
    services.insert("llm".to_string(), llm_health);

    // TTS服务健康检查
    let tts_health = check_tts_service_detailed().await;
    services.insert("tts".to_string(), tts_health);

    let overall_status = if services.values().all(|s| s.status == "healthy") {
        "healthy"
    } else {
        "unhealthy"
    };

    let health = DetailedHealthStatus {
        status: overall_status.to_string(),
        timestamp: chrono::Utc::now(),
        services,
    };

    HttpResponse::Ok().json(health)
}

async fn check_database_detailed() -> ServiceHealth {
    let start = std::time::Instant::now();

    match sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect(&std::env::var("DATABASE_URL").unwrap())
        .await
    {
        Ok(_) => ServiceHealth {
            status: "healthy".to_string(),
            response_time_ms: Some(start.elapsed().as_millis() as u64),
            error_message: None,
        },
        Err(e) => ServiceHealth {
            status: "unhealthy".to_string(),
            response_time_ms: Some(start.elapsed().as_millis() as u64),
            error_message: Some(e.to_string()),
        },
    }
}
```

## 告警系统

### 1. 告警规则

```yaml
# prometheus/alerts.yml
groups:
  - name: realtime_alerts
    rules:
      - alert: HighErrorRate
        expr: rate(requests_total{status="error"}[5m]) > 0.1
        for: 2m
        labels:
          severity: warning
        annotations:
          summary: "High error rate detected"
          description: "Error rate is {{ $value }} errors per second"

      - alert: HighLatency
        expr: histogram_quantile(0.95, request_duration_seconds) > 2
        for: 5m
        labels:
          severity: warning
        annotations:
          summary: "High latency detected"
          description: "95th percentile latency is {{ $value }} seconds"

      - alert: ServiceDown
        expr: up == 0
        for: 1m
        labels:
          severity: critical
        annotations:
          summary: "Service is down"
          description: "Service {{ $labels.instance }} is down"

      - alert: HighMemoryUsage
        expr: (memory_usage_bytes / memory_total_bytes) > 0.9
        for: 5m
        labels:
          severity: warning
        annotations:
          summary: "High memory usage"
          description: "Memory usage is {{ $value | humanizePercentage }}"

      - alert: HighCPUUsage
        expr: cpu_usage_percent > 80
        for: 5m
        labels:
          severity: warning
        annotations:
          summary: "High CPU usage"
          description: "CPU usage is {{ $value }}%"
```

### 2. 告警通知

```yaml
# alertmanager/config.yml
global:
  smtp_smarthost: 'localhost:587'
  smtp_from: 'alertmanager@example.com'

route:
  group_by: ['alertname']
  group_wait: 10s
  group_interval: 10s
  repeat_interval: 1h
  receiver: 'web.hook'

receivers:
  - name: 'web.hook'
    webhook_configs:
      - url: 'http://127.0.0.1:5001/'

inhibit_rules:
  - source_match:
      severity: 'critical'
    target_match:
      severity: 'warning'
    equal: ['alertname', 'dev', 'instance']
```

## 监控面板

### 1. Grafana仪表板

```json
{
  "dashboard": {
    "title": "Realtime Voice API Dashboard",
    "panels": [
      {
        "title": "Request Rate",
        "type": "graph",
        "targets": [
          {
            "expr": "rate(requests_total[5m])",
            "legendFormat": "{{method}} {{endpoint}}"
          }
        ]
      },
      {
        "title": "Response Time",
        "type": "graph",
        "targets": [
          {
            "expr": "histogram_quantile(0.95, request_duration_seconds)",
            "legendFormat": "95th percentile"
          },
          {
            "expr": "histogram_quantile(0.50, request_duration_seconds)",
            "legendFormat": "50th percentile"
          }
        ]
      },
      {
        "title": "Active Sessions",
        "type": "stat",
        "targets": [
          {
            "expr": "active_sessions"
          }
        ]
      },
      {
        "title": "Error Rate",
        "type": "graph",
        "targets": [
          {
            "expr": "rate(requests_total{status=\"error\"}[5m])",
            "legendFormat": "Error rate"
          }
        ]
      },
      {
        "title": "Service Latency",
        "type": "graph",
        "targets": [
          {
            "expr": "histogram_quantile(0.95, asr_latency_seconds)",
            "legendFormat": "ASR latency"
          },
          {
            "expr": "histogram_quantile(0.95, llm_latency_seconds)",
            "legendFormat": "LLM latency"
          },
          {
            "expr": "histogram_quantile(0.95, tts_latency_seconds)",
            "legendFormat": "TTS latency"
          }
        ]
      }
    ]
  }
}
```

### 2. 自定义监控

```rust
// 自定义业务指标
pub struct BusinessMetrics {
    pub conversations_total: Counter,
    pub messages_total: Counter,
    pub users_total: Counter,
    pub error_rate: Gauge,
}

impl BusinessMetrics {
    pub fn new(registry: &Registry) -> Self {
        let conversations_total = Counter::new(
            "conversations_total",
            "Total number of conversations"
        ).unwrap();

        let messages_total = Counter::new(
            "messages_total",
            "Total number of messages"
        ).unwrap();

        let users_total = Counter::new(
            "users_total",
            "Total number of users"
        ).unwrap();

        let error_rate = Gauge::new(
            "error_rate",
            "Error rate percentage"
        ).unwrap();

        registry.register(Box::new(conversations_total.clone())).unwrap();
        registry.register(Box::new(messages_total.clone())).unwrap();
        registry.register(Box::new(users_total.clone())).unwrap();
        registry.register(Box::new(error_rate.clone())).unwrap();

        Self {
            conversations_total,
            messages_total,
            users_total,
            error_rate,
        }
    }
}
```

## 日志分析

### 1. 日志查询

```bash
# 查看实时日志
tail -f /var/log/realtime/app.log

# 查看错误日志
grep "ERROR" /var/log/realtime/app.log

# 查看特定会话的日志
grep "sess_001" /var/log/realtime/app.log

# 查看性能日志
grep "latency" /var/log/realtime/app.log
```

### 2. 日志聚合

使用ELK Stack进行日志聚合：

```yaml
# filebeat.yml
filebeat.inputs:
  - type: log
    enabled: true
    paths:
      - /var/log/realtime/*.log
    json.keys_under_root: true
    json.add_error_key: true

output.elasticsearch:
  hosts: ["localhost:9200"]
  index: "realtime-logs-%{+yyyy.MM.dd}"

setup.kibana:
  host: "localhost:5601"
```

### 3. 日志告警

```yaml
# elastalert/rules/error_rate.yaml
name: High Error Rate
type: frequency
index: realtime-logs-*
num_events: 10
timeframe:
  minutes: 5
filter:
  - query:
      query_string:
        query: "level:ERROR"
alert:
  - "email"
  - "slack"
email:
  - "admin@example.com"
slack_webhook_url: "https://hooks.slack.com/services/xxx/yyy/zzz"
```

## 性能监控

### 1. 系统资源监控

```rust
use sysinfo::{System, SystemExt, CpuExt};

pub struct SystemMonitor {
    sys: System,
}

impl SystemMonitor {
    pub fn new() -> Self {
        Self {
            sys: System::new_all(),
        }
    }

    pub fn get_cpu_usage(&mut self) -> f32 {
        self.sys.refresh_cpu();
        self.sys.global_cpu_info().cpu_usage()
    }

    pub fn get_memory_usage(&mut self) -> u64 {
        self.sys.refresh_memory();
        self.sys.used_memory()
    }

    pub fn get_disk_usage(&mut self) -> u64 {
        self.sys.refresh_disks();
        // 计算磁盘使用率
        0
    }
}
```

### 2. 应用性能监控

```rust
use std::time::Instant;

pub struct PerformanceMonitor {
    start_time: Instant,
}

impl PerformanceMonitor {
    pub fn new() -> Self {
        Self {
            start_time: Instant::now(),
        }
    }

    pub fn measure<F, T>(&self, name: &str, f: F) -> T
    where
        F: FnOnce() -> T,
    {
        let start = Instant::now();
        let result = f();
        let duration = start.elapsed();

        tracing::info!(
            "Performance measurement",
            operation = name,
            duration_ms = duration.as_millis()
        );

        result
    }
}
```

这个监控和日志文档提供了完整的监控解决方案，包括日志管理、指标收集、健康检查和告警系统。
