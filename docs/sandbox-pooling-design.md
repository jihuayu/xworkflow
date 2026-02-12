# 沙箱预热与复用设计

## 1. 问题分析

### 1.1 JavaScript 沙箱（boa_engine）

**当前实现**（`crates/xworkflow-sandbox-js/src/sandbox.rs`）：

每次 Code 节点执行时：
```
1. Context::default()           — 创建新的 JS 运行时上下文 (~10-30ms)
2. register_all(&mut context)   — 注册内建函数 (crypto, uuid, btoa 等) (~1-5ms)
3. context.eval(user_code)      — 编译 + 执行用户代码 (~1-50ms)
4. 提取返回值                    — Segment 转换
5. Context 被丢弃               — 完整 GC + 析构
```

**问题**：步骤 1-2 是固定开销，每次执行都重复。对于 iteration 节点循环 100 次调用同一段 JS 代码，Context 创建被重复 100 次。

### 1.2 WASM 沙箱（wasmtime）

**当前实现**（`crates/xworkflow-sandbox-wasm/src/sandbox.rs`）：

每次执行时：
```
1. Engine 已复用（WasmSandbox 持有）          — 零成本
2. Module::new(&engine, wasm_bytes)           — 编译 WASM 字节码 (~1-5ms)
3. Store::new(&engine, StoreLimits)           — 创建执行环境 (~0.1ms)
4. Instance::new(&mut store, &module, &[])    — 实例化 (~0.1ms)
5. 调用导出函数                               — 执行
6. Store 被丢弃
```

**问题**：步骤 2 是最重的操作。同一段 WASM 代码在不同节点执行中被重复编译。

### 1.3 流式 JS 运行时

**当前实现**（`crates/xworkflow-sandbox-js/src/streaming.rs`）：

```
1. spawn_blocking() 创建专用线程
2. Context::default() + register_all()       — 同上
3. 执行初始化代码，提取 stream callbacks
4. 通过 mpsc channel 接收 chunk → 调用 JS callback
5. finalize 时销毁
```

**问题**：每个流式 Code 节点都创建独立的 blocking 线程 + JS 上下文。

---

## 2. 设计方案

### 2.1 JS Context 池化

#### 池结构

```rust
pub struct JsContextPool {
    /// 空闲 Context 队列
    idle: Mutex<Vec<PooledContext>>,
    /// 池配置
    config: PoolConfig,
    /// 统计信息
    stats: Arc<PoolStats>,
}

struct PooledContext {
    context: Context,
    /// 上次使用时间（用于过期淘汰）
    last_used: Instant,
    /// 累计使用次数
    use_count: u64,
}

pub struct PoolConfig {
    /// 池中最大空闲 Context 数
    pub max_idle: usize,           // 默认: 8
    /// 单个 Context 最大复用次数（防止内存泄漏）
    pub max_reuse_count: u64,      // 默认: 1000
    /// 空闲超时（超过后销毁）
    pub idle_timeout: Duration,    // 默认: 60s
}
```

#### 获取/归还流程

```
acquire():
  1. idle.lock() → 尝试 pop 一个空闲 Context
  2. 检查：use_count < max_reuse_count && !expired
  3. 如果有合适的 → 返回
  4. 如果没有 → Context::default() + register_all() 创建新的

release(context):
  1. 重置 Context 全局状态（清除用户定义的变量/函数）
  2. idle.lock() → push
  3. 如果 idle.len() > max_idle → 丢弃最旧的
```

#### Context 重置策略

boa_engine 的 `Context` 重置是核心挑战。需要在复用时清除用户代码的副作用：

**方案 A：Realm 级别隔离（推荐）**
- boa_engine 支持多 Realm（类似浏览器 iframe）
- 每次执行创建新 Realm，共享底层 Engine
- Realm 创建远比 Context 创建便宜
- 执行完毕后丢弃 Realm，保留 Context

**方案 B：全局变量清理**
- 执行前记录 global 对象的 property 列表
- 执行后删除新增的 properties
- 风险：某些副作用无法完全清除（闭包、WeakRef 等）

**方案 C：Copy-on-Write 快照**
- 初始化后对 Context 状态做快照
- 每次使用后恢复快照
- 依赖 boa_engine 是否支持此功能

**推荐方案 A**，若 boa 版本不支持 Realm 则退回方案 B + max_reuse_count 限制。

### 2.2 WASM 模块缓存

#### 缓存结构

```rust
pub struct WasmModuleCache {
    /// code_hash → 编译后的 Module
    cache: Mutex<HashMap<u64, CachedModule>>,
    /// 缓存配置
    config: ModuleCacheConfig,
}

struct CachedModule {
    module: Module,
    /// 最后访问时间
    last_accessed: Instant,
    /// 访问计数
    access_count: u64,
}

pub struct ModuleCacheConfig {
    /// 最大缓存模块数
    pub max_entries: usize,         // 默认: 64
    /// 模块过期时间
    pub ttl: Duration,              // 默认: 300s
    /// 最大单模块 WASM 大小
    pub max_module_size: usize,     // 默认: 1MB
}
```

#### 查找流程

```
get_or_compile(engine, wasm_bytes):
  1. hash = xxhash(wasm_bytes)
  2. cache.lock() → 查找 hash
  3. 命中 → 更新 last_accessed, 返回 module.clone()
  4. 未命中 → Module::new(engine, wasm_bytes)
  5. 插入缓存，若超过 max_entries → LRU 淘汰
  6. 返回 module
```

**注意**：`wasmtime::Module` 实现了 `Clone`（内部 Arc 共享编译产物），clone 是 O(1)。

#### 缓存键设计

使用 WASM 字节码的哈希作为缓存键：

```rust
fn compute_cache_key(wasm_bytes: &[u8]) -> u64 {
    // xxHash 速度极快，适合大块数据哈希
    xxhash_rust::xxh3::xxh3_64(wasm_bytes)
}
```

对于文本格式的 code 节点（user code string → compiled WASM），使用代码字符串的哈希：

```rust
fn compute_code_key(code: &str, language: &str) -> u64 {
    let mut hasher = XxHash64::default();
    hasher.write(language.as_bytes());
    hasher.write(b":");
    hasher.write(code.as_bytes());
    hasher.finish()
}
```

### 2.3 WASM Store 池化

Store 比 Module 轻量，但仍有创建成本。对于高频执行场景：

```rust
pub struct WasmStorePool {
    idle: Mutex<Vec<Store<StoreLimits>>>,
    engine: Engine,
    config: StorePoolConfig,
}

pub struct StorePoolConfig {
    pub max_idle: usize,      // 默认: 4
    pub max_fuel: u64,        // 每次执行的 fuel 限制
    pub max_memory_pages: u32,
}
```

**Store 重置**：
```
release(store):
  1. store.set_fuel(max_fuel)        — 重置执行预算
  2. 检查 memory 使用量，超限则丢弃
  3. idle.push(store)
```

**注意**：Store 的 memory 重置依赖 wasmtime API 的支持。如果无法安全重置，则放弃池化，只做 Module 缓存。

### 2.4 SandboxManager 扩展

当前 `SandboxManager` 结构（`src/sandbox/manager.rs`）：

```rust
pub struct SandboxManager {
    default_sandbox: Option<Arc<dyn CodeSandbox>>,
    sandboxes: HashMap<CodeLanguage, Arc<dyn CodeSandbox>>,
    config: SandboxManagerConfig,
}
```

扩展为带池化的管理器：

```rust
pub struct SandboxManager {
    default_sandbox: Option<Arc<dyn CodeSandbox>>,
    sandboxes: HashMap<CodeLanguage, Arc<dyn CodeSandbox>>,
    config: SandboxManagerConfig,
    /// JS Context 池（仅 builtin-sandbox-js feature 启用）
    #[cfg(feature = "builtin-sandbox-js")]
    js_context_pool: Arc<JsContextPool>,
    /// WASM 模块缓存（仅 builtin-sandbox-wasm feature 启用）
    #[cfg(feature = "builtin-sandbox-wasm")]
    wasm_module_cache: Arc<WasmModuleCache>,
}
```

#### CodeSandbox Trait 扩展

在 trait 中添加可选的池化支持（`crates/xworkflow-types/src/sandbox.rs`）：

```rust
#[async_trait]
pub trait CodeSandbox: Send + Sync {
    // ... 现有方法

    /// 预热沙箱（可选，用于启动时预创建 Context）
    async fn warmup(&self, count: usize) -> Result<(), SandboxError> {
        Ok(()) // 默认不做预热
    }

    /// 获取池统计信息（可选）
    async fn pool_stats(&self) -> Option<PoolStats> {
        None
    }
}
```

### 2.5 安全考量

#### 多租户隔离

启用 `security` feature 时，不同 ResourceGroup 的执行必须隔离：

```rust
// 方案：per-ResourceGroup 的池
pub struct IsolatedJsContextPool {
    pools: HashMap<ResourceGroupId, JsContextPool>,
    default_pool: JsContextPool,
}
```

或者更简单地：在 security 模式下禁用 Context 复用，每次创建新 Context：

```rust
impl JsContextPool {
    fn acquire(&self, security_level: SecurityLevel) -> PooledContext {
        match security_level {
            SecurityLevel::Strict => {
                // 严格模式：不复用，总是创建新 Context
                PooledContext::new_fresh()
            }
            SecurityLevel::Standard | SecurityLevel::Relaxed => {
                // 标准/宽松模式：从池中获取
                self.try_acquire_from_pool().unwrap_or_else(PooledContext::new_fresh)
            }
        }
    }
}
```

#### 资源泄漏防护

- max_reuse_count 限制单个 Context 的复用次数
- idle_timeout 自动清理长期未使用的 Context
- 可选：定期 full GC（boa `context.run_gc()`）

---

## 3. 接口定义

### 3.1 PoolConfig

```rust
/// 沙箱池化配置
pub struct SandboxPoolConfig {
    pub js: JsPoolConfig,
    pub wasm: WasmPoolConfig,
}

pub struct JsPoolConfig {
    pub enabled: bool,
    pub max_idle: usize,
    pub max_reuse_count: u64,
    pub idle_timeout_secs: u64,
    pub warmup_count: usize,
}

pub struct WasmPoolConfig {
    pub module_cache_enabled: bool,
    pub max_cached_modules: usize,
    pub module_ttl_secs: u64,
    pub store_pool_enabled: bool,
    pub max_idle_stores: usize,
}
```

### 3.2 统计接口

```rust
pub struct PoolStats {
    pub idle_count: usize,
    pub total_created: u64,
    pub total_reused: u64,
    pub total_evicted: u64,
    pub cache_hit_rate: f64,     // WASM module cache
}
```

---

## 4. 迁移策略

### 阶段 1：WASM 模块缓存（最简单，收益明确）
1. 在 `WasmSandbox` 中添加 `HashMap<u64, Module>` 缓存
2. execute() 中先查缓存再编译
3. 添加 benchmark 验证

### 阶段 2：JS Context 池
1. 实现 `JsContextPool` 基础结构
2. 在 `BuiltinSandbox::execute_js()` 中使用池
3. 验证状态隔离的正确性
4. Iteration 节点循环场景测试

### 阶段 3：流式 JS 运行时优化
1. 流式运行时复用预热的 Context
2. 减少 spawn_blocking 线程创建

### 阶段 4：预热与监控
1. 添加 `warmup()` 接口
2. 集成 PoolStats 到健康检查
3. 可选：expose metrics

---

## 5. 预期收益

| 场景 | 当前耗时 | 优化后 | 提升 |
|------|---------|-------|------|
| 首次 JS 执行 | 15-35ms | 15-35ms（冷启动不变） | 0% |
| 后续 JS 执行（池复用） | 15-35ms | 1-5ms（跳过 Context 创建） | 75-90% |
| Iteration × 100 JS 调用 | 1.5-3.5s | 100-500ms | 85-95% |
| 首次 WASM 执行 | 2-6ms | 2-6ms（冷启动不变） | 0% |
| 同代码 WASM 重复执行 | 2-6ms | 0.2-0.5ms（缓存命中） | 90%+ |

**最大受益场景**：Iteration/Loop 节点中循环调用相同 Code 节点。

---

## 6. 风险评估

| 风险 | 影响 | 缓解措施 |
|------|------|---------|
| Context 状态泄漏 | 安全问题：前一执行的变量残留 | max_reuse_count + Realm 隔离 |
| 内存增长 | 池中 Context 占用内存 | max_idle 限制 + idle_timeout 淘汰 |
| boa API 变更 | Context 重置方法不稳定 | 版本锁定，方案 B 作为 fallback |
| WASM 缓存失效 | 代码变更后仍使用旧 Module | 基于代码哈希的缓存键，内容变更 = key 变更 |
| 并发竞争 | 多个节点同时 acquire | Mutex 保护池操作 |

---

## 7. 关键文件

| 文件 | 改动 |
|------|------|
| `crates/xworkflow-sandbox-js/src/sandbox.rs` | 添加 JsContextPool，修改 execute_js |
| `crates/xworkflow-sandbox-js/src/streaming.rs` | 复用池中 Context |
| `crates/xworkflow-sandbox-wasm/src/sandbox.rs` | 添加 WasmModuleCache |
| `src/sandbox/manager.rs` | 持有池引用，透传配置 |
| `crates/xworkflow-types/src/sandbox.rs` | 扩展 CodeSandbox trait (warmup, pool_stats) |
