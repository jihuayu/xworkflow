# HTTP 连接池设计

## 1. 问题分析

### 1.1 当前实现

HTTP 请求节点（`src/nodes/data_transform.rs:654-686`）每次执行都创建新的 `reqwest::Client`：

```rust
// 非安全模式
let client = reqwest::Client::builder()
    .timeout(Duration::from_secs(timeout))
    .build()?;

// 安全模式
let client = SecureHttpClientFactory::build(policy, timeout)?;
```

**每次创建的开销**：
- TLS 初始化：~1-5ms
- 连接池数据结构分配：~0.1ms
- DNS resolver 初始化：~0.1ms
- 首次请求 TCP 握手 + TLS 握手：~10-50ms（取决于网络延迟）
- **总计**：~11-56ms/请求（含首次连接建立）

### 1.2 对比：已有的正确模式

OpenAI provider（`src/llm/provider/openai.rs:24-34`）正确地复用了 client：

```rust
pub struct OpenAiProvider {
    config: OpenAiConfig,
    client: reqwest::Client,  // 创建时初始化，后续复用
}
```

这意味着 LLM 节点的 HTTP 连接是复用的，但 HTTP 请求节点的连接每次都重新建立。

### 1.3 reqwest 内建连接池

`reqwest::Client` 内部已经维护了 hyper 连接池：
- 同一 `Client` 实例对同一 host 的请求会复用 TCP/TLS 连接
- 默认 keep-alive 超时 90s
- 默认最大空闲连接数：无限制

**关键**：要利用这个内建连接池，必须**复用同一个 Client 实例**。当前每次创建新 Client，连接池随旧 Client 一起被丢弃。

---

## 2. 设计方案

### 2.1 核心思路

将 HTTP client 从 per-request 创建改为按**资源组（ResourceGroup）** 隔离复用：
- 同一 `group_id` 内的工作流共享连接池
- 不同 `group_id` 之间连接完全隔离（安全 + 配额独立）
- 无安全模式时退回全局单例

**隔离维度**：`(group_id, network_policy_hash)` 二元组。同一资源组 + 同一网络策略 = 共享 client。

### 2.2 隔离必要性分析

为什么必须按 ResourceGroup 隔离：

| 关注点 | 说明 |
|--------|------|
| **连接安全** | 不同租户不应共享 TCP 连接——共享连接理论上可能泄露 Host/Cookie 等 header |
| **DNS 策略** | 不同 ResourceGroup 可能有不同的 `NetworkPolicy`（IP 白名单、SSRF 规则），需要不同的 `SecureDnsResolver` |
| **速率限制** | `ResourceQuota.http_rate_limit_per_minute` 按 group_id 计数。共享 client 不影响 ResourceGovernor 的计数（计数在 governor 层），但连接池共享可能导致一个组耗尽连接影响另一个组 |
| **凭证隔离** | 不同组的 `credential_refs` 不同，共享 client 时 cookie/auth 可能串扰 |
| **审计追踪** | per-group client 便于监控每个组的连接数、请求量 |

### 2.3 HttpClientProvider 架构

```rust
/// HTTP 客户端提供者，按资源组隔离
pub struct HttpClientProvider {
    /// 标准模式客户端（security feature 关闭时使用）
    #[cfg(not(feature = "security"))]
    standard_client: reqwest::Client,

    /// 安全模式：per-group 的客户端缓存
    /// key = (group_id, network_policy_hash)
    #[cfg(feature = "security")]
    group_clients: Mutex<HashMap<GroupClientKey, GroupClientEntry>>,

    /// 配置
    config: HttpPoolConfig,
}

/// 缓存 key：资源组 + 网络策略
#[cfg(feature = "security")]
#[derive(Hash, Eq, PartialEq, Clone)]
struct GroupClientKey {
    group_id: String,
    policy_hash: u64,   // NetworkPolicy 的哈希值
}

/// 缓存条目
#[cfg(feature = "security")]
struct GroupClientEntry {
    client: reqwest::Client,
    created_at: Instant,
    last_used: Instant,
    request_count: u64,
}
```

### 2.4 配置

```rust
pub struct HttpPoolConfig {
    /// 连接池最大空闲连接数（per host per client）
    pub pool_max_idle_per_host: usize,     // 默认: 10
    /// 空闲连接超时
    pub pool_idle_timeout: Duration,        // 默认: 90s
    /// 默认请求超时（可被节点配置覆盖）
    pub default_timeout: Duration,          // 默认: 30s
    /// TCP keepalive 间隔
    pub tcp_keepalive: Option<Duration>,    // 默认: 60s
    /// 是否启用 HTTP/2
    pub http2_enabled: bool,                // 默认: true
    /// 最大缓存的 group client 数量
    #[cfg(feature = "security")]
    pub max_group_clients: usize,           // 默认: 32
    /// group client 空闲过期时间（超过后销毁，释放连接）
    #[cfg(feature = "security")]
    pub group_client_idle_timeout: Duration, // 默认: 300s
}
```

### 2.5 Client 获取流程

```
get_client(context):
  ┌──────────────────────────────────────┐
  │ security feature 关闭?               │
  │   → 返回 standard_client            │
  └──────────┬───────────────────────────┘
             │ security 开启
             ▼
  ┌──────────────────────────────────────┐
  │ context.resource_group() 存在?       │
  │   否 → 创建临时 client（不缓存）     │
  └──────────┬───────────────────────────┘
             │ 存在 group
             ▼
  ┌──────────────────────────────────────┐
  │ 构建 key = (group_id, policy_hash)  │
  │ group_clients.lock()                │
  │   命中 → 更新 last_used, 返回       │
  │   未命中 → 创建新 client            │
  │     NetworkPolicy 存在?              │
  │       是 → SecureDnsResolver(policy) │
  │       否 → 默认 resolver             │
  │     插入缓存                         │
  │     缓存满? → LRU 淘汰              │
  │     返回 client                     │
  └──────────────────────────────────────┘
```

### 2.6 核心实现

```rust
impl HttpClientProvider {
    pub fn new(config: HttpPoolConfig) -> Self {
        Self {
            #[cfg(not(feature = "security"))]
            standard_client: Self::build_default_client(&config),
            #[cfg(feature = "security")]
            group_clients: Mutex::new(HashMap::new()),
            config,
        }
    }

    fn build_default_client(config: &HttpPoolConfig) -> reqwest::Client {
        reqwest::Client::builder()
            .pool_max_idle_per_host(config.pool_max_idle_per_host)
            .pool_idle_timeout(config.pool_idle_timeout)
            .tcp_keepalive(config.tcp_keepalive)
            .timeout(config.default_timeout)
            .build()
            .expect("Failed to create HTTP client")
    }

    /// 获取 client（非安全模式）
    #[cfg(not(feature = "security"))]
    pub fn client_for_context(&self, _context: &RuntimeContext) -> reqwest::Client {
        self.standard_client.clone()  // Arc clone, O(1)
    }

    /// 获取 client（安全模式，按资源组隔离）
    #[cfg(feature = "security")]
    pub fn client_for_context(
        &self,
        context: &RuntimeContext,
    ) -> Result<reqwest::Client, NodeError> {
        let group = context.resource_group();
        let policy = context.security_policy();

        // 无资源组 → 创建临时 client（不缓存，用完即弃）
        let Some(group) = group else {
            return Ok(Self::build_client_with_policy(&self.config, policy));
        };

        let key = GroupClientKey {
            group_id: group.group_id.clone(),
            policy_hash: policy.map(|p| p.network_hash()).unwrap_or(0),
        };

        let mut cache = self.group_clients.lock();

        // 命中缓存
        if let Some(entry) = cache.get_mut(&key) {
            entry.last_used = Instant::now();
            entry.request_count += 1;
            return Ok(entry.client.clone());
        }

        // 创建新 client
        let client = Self::build_client_with_policy(&self.config, policy);

        // LRU 淘汰：超过上限时移除最久未使用的
        if cache.len() >= self.config.max_group_clients {
            let oldest_key = cache.iter()
                .min_by_key(|(_, entry)| entry.last_used)
                .map(|(k, _)| k.clone());
            if let Some(k) = oldest_key {
                cache.remove(&k);
            }
        }

        cache.insert(key, GroupClientEntry {
            client: client.clone(),
            created_at: Instant::now(),
            last_used: Instant::now(),
            request_count: 1,
        });

        Ok(client)
    }

    #[cfg(feature = "security")]
    fn build_client_with_policy(
        config: &HttpPoolConfig,
        policy: Option<&SecurityPolicy>,
    ) -> reqwest::Client {
        match policy.and_then(|p| p.network.as_ref()) {
            Some(network_policy) => {
                // 带安全 DNS resolver 的 client
                reqwest::Client::builder()
                    .pool_max_idle_per_host(config.pool_max_idle_per_host)
                    .pool_idle_timeout(config.pool_idle_timeout)
                    .tcp_keepalive(config.tcp_keepalive)
                    .dns_resolver(Arc::new(SecureDnsResolver::new(network_policy)))
                    .build()
                    .expect("Failed to create secure HTTP client")
            }
            None => Self::build_default_client(config),
        }
    }

    /// 清理过期的 group client（可由定时器调用）
    #[cfg(feature = "security")]
    pub fn cleanup_expired(&self) {
        let mut cache = self.group_clients.lock();
        let timeout = self.config.group_client_idle_timeout;
        cache.retain(|_, entry| entry.last_used.elapsed() < timeout);
    }

    /// 获取统计信息
    #[cfg(feature = "security")]
    pub fn stats(&self) -> HttpPoolStats {
        let cache = self.group_clients.lock();
        HttpPoolStats {
            active_groups: cache.len(),
            entries: cache.iter().map(|(k, v)| GroupClientStats {
                group_id: k.group_id.clone(),
                request_count: v.request_count,
                idle_secs: v.last_used.elapsed().as_secs(),
            }).collect(),
        }
    }
}
```

### 2.7 SecurityPolicy 添加 network_hash

```rust
// src/security/policy.rs
impl SecurityPolicy {
    /// 计算 NetworkPolicy 部分的哈希（用于 client 缓存 key）
    pub fn network_hash(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        use std::collections::hash_map::DefaultHasher;
        let mut hasher = DefaultHasher::new();
        // 对影响 client 行为的字段求哈希
        if let Some(ref net) = self.network {
            net.allowed_hosts.hash(&mut hasher);
            net.blocked_ips.hash(&mut hasher);
            net.allowed_ip_ranges.hash(&mut hasher);
            net.allow_private_ip.hash(&mut hasher);
        }
        hasher.finish()
    }
}
```

同一 ResourceGroup 如果 NetworkPolicy 变更（如动态更新白名单），hash 不同 → 自动创建新 client。

### 2.8 Per-Request 超时

共享 client 后，超时在 request 级别设置：

```rust
// 当前（client-level timeout）：
let client = reqwest::Client::builder()
    .timeout(Duration::from_secs(node_timeout))
    .build()?;
let resp = client.get(url).send().await?;

// 改为（request-level timeout）：
let client = provider.client_for_context(context)?;
let resp = client.get(url)
    .timeout(Duration::from_secs(node_timeout))  // per-request timeout
    .send()
    .await?;
```

### 2.9 HTTP 节点执行器改造

```rust
#[async_trait]
impl NodeExecutor for HttpRequestExecutor {
    async fn execute(
        &self,
        node_id: &str,
        config: &Value,
        variable_pool: &VariablePool,
        context: &RuntimeContext,
    ) -> Result<NodeRunResult, NodeError> {
        // ... 解析 config ...

        // 获取按资源组隔离的 client
        let client = match context.http_client() {
            Some(provider) => provider.client_for_context(context)?,
            None => {
                // 回退：创建临时 client
                reqwest::Client::builder()
                    .timeout(Duration::from_secs(timeout))
                    .build()
                    .map_err(|e| NodeError::HttpError(e.to_string()))?
            }
        };

        // per-request timeout
        let resp = client.request(method, &url)
            .timeout(Duration::from_secs(timeout))
            .headers(headers)
            .body(body)
            .send()
            .await?;
        // ...
    }
}
```

### 2.10 注入位置

`HttpClientProvider` 放在 `RuntimeGroup` 层级（跨工作流共享），而非 `WorkflowContext`（单次执行）：

```rust
// src/core/runtime_group.rs
pub struct RuntimeGroup {
    // ... 现有字段
    pub http_client_provider: Option<Arc<HttpClientProvider>>,
}

// Builder
impl RuntimeGroupBuilder {
    pub fn http_pool_config(mut self, config: HttpPoolConfig) -> Self {
        self.group.http_client_provider = Some(Arc::new(
            HttpClientProvider::new(config)
        ));
        self
    }
}
```

多个工作流执行共享同一个 `HttpClientProvider` → 同 group_id 的所有工作流复用连接池。

RuntimeContext 通过 RuntimeGroup 间接访问：

```rust
impl RuntimeContext {
    pub fn http_client(&self) -> Option<&Arc<HttpClientProvider>> {
        self.runtime_group()
            .and_then(|rg| rg.http_client_provider.as_ref())
    }
}
```

### 2.11 隔离效果示意

```
Platform
├── ResourceGroup "tenant-A" (group_id="A")
│   ├── Workflow 1  ──┐
│   ├── Workflow 2  ──┼──→ 共享 reqwest::Client (A)
│   └── Workflow 3  ──┘      └── 连接池: api.example.com ×3 idle
│
├── ResourceGroup "tenant-B" (group_id="B")
│   ├── Workflow 4  ──┐
│   └── Workflow 5  ──┼──→ 共享 reqwest::Client (B)  ← 独立连接池
│                     ┘      └── 连接池: api.example.com ×2 idle
│
└── 无 ResourceGroup（测试/开发）
    └── Workflow 6  ──────→ 临时 reqwest::Client（不缓存）
```

---

## 3. 迁移策略

### 阶段 1：基础设施
1. 新增 `HttpClientProvider`、`HttpPoolConfig`、`GroupClientKey`
2. 实现 `client_for_context()` 的双模式（security on/off）
3. 添加到 `RuntimeGroup`

### 阶段 2：HTTP 节点改造
1. `HttpRequestExecutor::execute()` 通过 context 获取 client
2. 改用 per-request timeout
3. 保留临时 client 的回退路径

### 阶段 3：SecurityPolicy 适配
1. 添加 `network_hash()` 方法
2. `NetworkPolicy` 相关字段实现 `Hash`
3. 验证 policy 变更 → 新 client 的正确性

### 阶段 4：生命周期管理
1. 实现 `cleanup_expired()` 定时清理
2. 在 `RuntimeGroup::drop()` 或 shutdown 时清理所有 client
3. 添加 `stats()` 监控接口

---

## 4. 预期收益

| 场景 | 当前耗时 | 优化后 | 提升 |
|------|---------|-------|------|
| 首次请求 (cold) | 15-55ms | 15-55ms | 0%（首次仍需建连） |
| 同 group 同 host 后续请求 | 15-55ms | 1-5ms（连接复用） | 90%+ |
| 同 group 10 个 HTTP 节点访问同 host | 150-550ms | 15-50ms + 9×(1-5ms) | 85%+ |
| 不同 group 同 host | 15-55ms/each | 各自首次建连，组内复用 | 组内 90%+ |
| 不同 host 请求 | 15-55ms/each | 略有改善（TLS 会话缓存） | ~10% |

**最大受益场景**：同一资源组内多个工作流/多个 HTTP 节点访问同一 API 服务。

---

## 5. 风险评估

| 风险 | 影响 | 缓解措施 |
|------|------|---------|
| group client 内存增长 | 大量 group 时缓存占用增加 | `max_group_clients` 限制 + LRU 淘汰 |
| 空闲 group 连接不释放 | 长期运行时连接数增长 | `group_client_idle_timeout` + `cleanup_expired()` |
| 跨 group 连接泄漏 | 安全隔离失效 | key 包含 group_id，不同 group 绝不共享 |
| NetworkPolicy 动态变更 | 旧 client 的 DNS resolver 过时 | key 包含 policy_hash，变更自动创建新 client |
| 超时行为变化 | client-level vs request-level | 显式设置 per-request timeout |
| 无 ResourceGroup 时的行为 | 每次创建临时 client | 文档说明：生产环境应配置 ResourceGroup |

---

## 6. 关键文件

| 文件 | 改动 |
|------|------|
| 新文件 `src/core/http_client.rs` | `HttpClientProvider`、`HttpPoolConfig`、`GroupClientKey` |
| `src/core/runtime_group.rs` | 添加 `http_client_provider` 字段 + builder |
| `src/core/runtime_context.rs` | 添加 `http_client()` 透传方法 |
| `src/nodes/data_transform.rs` | `HttpRequestExecutor` 使用 `client_for_context()` |
| `src/security/policy.rs` | 添加 `network_hash()` 方法 |
| `src/security/network.rs` | `NetworkPolicy` 相关字段实现 `Hash` |
