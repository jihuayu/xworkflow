# JS 沙箱内置 API 设计文档（Rust 绑定）

## Context

xworkflow 的 JS 沙箱（`src/sandbox/builtin.rs`）基于 boa_engine 0.20，当前只注入了 `console` 对象。用户代码缺少常用工具函数（时间、UUID、加密、Base64、随机数），需要通过 Rust 原生绑定注入到 JS 运行时，既保证性能又保证安全。

Cargo.toml 已有的相关依赖：`uuid 1.6`、`chrono 0.4`、`base64 0.22`、`boa_engine 0.20`。

---

## 一、JS API 设计

### 1.1 时间 — `datetime` 全局对象

```javascript
datetime.now()       // → 1700000000000 (毫秒时间戳)
datetime.timestamp() // → 1700000000 (秒时间戳)
datetime.format(fmt) // → "2024-01-15 10:30:00" (chrono格式，默认 "%Y-%m-%d %H:%M:%S")
datetime.isoString() // → "2024-01-15T10:30:00.000Z"
```

### 1.2 JSON — 已内置，无需额外添加

boa_engine 已内置 `JSON.parse()` / `JSON.stringify()`，无需改动。

### 1.3 加密/哈希 — `crypto` 全局对象

```javascript
// 哈希
crypto.md5(str)              // → hex string
crypto.sha1(str)             // → hex string
crypto.sha256(str)           // → hex string
crypto.sha512(str)           // → hex string

// HMAC
crypto.hmacSha256(key, data) // → hex string
crypto.hmacSha512(key, data) // → hex string

// AES-256-GCM 加密/解密
crypto.aesEncrypt(plaintext, key_hex)
  // → { ciphertext: base64, iv: base64 }
crypto.aesDecrypt(ciphertext_base64, key_hex, iv_base64)
  // → plaintext string
```

### 1.4 Base64 — 全局函数（标准 Web API 命名）

```javascript
btoa(str)        // → base64 编码字符串
atob(base64_str) // → 解码后字符串
```

### 1.5 UUID — 全局函数

```javascript
uuidv4() // → "550e8400-e29b-41d4-a716-446655440000"
```

### 1.6 随机数 — 全局函数

```javascript
randomInt(min, max)   // → [min, max] 闭区间随机整数
randomFloat()         // → [0.0, 1.0) 随机浮点数（补充 Math.random()）
randomBytes(length)   // → hex string，length 字节的随机数据
```

---

## 二、新增依赖（Cargo.toml）

```toml
sha1 = "0.10"
sha2 = "0.10"
md-5 = "0.10"
hmac = "0.12"
aes-gcm = "0.10"
rand = "0.8"
hex = "0.4"
```

---

## 三、文件改动清单

| 文件 | 改动 |
|------|------|
| `Cargo.toml` | 添加 7 个新依赖 |
| `src/sandbox/js_builtins.rs` | **新增**：所有原生函数实现 + `register_all(ctx)` 入口 |
| `src/sandbox/mod.rs` | 添加 `pub mod js_builtins;` |
| `src/sandbox/builtin.rs` | 在 `execute_js` 的 `Context::default()` 之后调用 `js_builtins::register_all(&mut context)` |

---

## 四、`src/sandbox/js_builtins.rs` 实现结构

```rust
use boa_engine::{Context, JsResult, JsValue, NativeFunction, js_string, property::Attribute};
use boa_engine::object::ObjectInitializer;

/// 注册所有内置 API 到 boa context
pub fn register_all(context: &mut Context) -> JsResult<()> {
    register_datetime(context)?;
    register_crypto(context)?;
    register_base64(context)?;
    register_uuid(context)?;
    register_random(context)?;
    Ok(())
}
```

### 4.1 datetime 注册

用 `ObjectInitializer` 创建 `datetime` 对象，包含 4 个方法。内部使用 `chrono::Utc::now()`。

### 4.2 crypto 注册

用 `ObjectInitializer` 创建 `crypto` 对象，包含 8 个方法。
- hash 系列：读取第一个参数为 string → 计算 hash → 返回 hex string
- HMAC：读取 key + data → 计算 HMAC → 返回 hex string
- AES：
  - encrypt：生成随机 12 字节 nonce，AES-256-GCM 加密，返回 JS 对象 `{ ciphertext, iv }`
  - decrypt：解析 base64 的 ciphertext 和 iv，用 key 解密，返回明文 string

### 4.3 base64 注册

两个全局函数 `btoa` / `atob`，使用 `base64::engine::general_purpose::STANDARD`。

### 4.4 uuid 注册

全局函数 `uuidv4`，调用 `uuid::Uuid::new_v4().to_string()`。

### 4.5 random 注册

三个全局函数：
- `randomInt(min, max)` — `rand::thread_rng().gen_range(min..=max)`
- `randomFloat()` — `rand::thread_rng().gen::<f64>()`
- `randomBytes(len)` — 生成 len 字节随机数据返回 hex

---

## 五、`builtin.rs` 改动点

在 `execute_js` 方法中，`Context::default()` 之后、console 注入之前，调用：

```rust
// 文件: src/sandbox/builtin.rs, execute_js 方法, 约 line 97
fn execute_js(&self, code: &str, inputs: &Value, timeout: Duration) -> Result<SandboxResult, SandboxError> {
    let start_time = Instant::now();
    let mut context = Context::default();

    // ← 新增：注册内置 API
    super::js_builtins::register_all(&mut context)
        .map_err(|e| SandboxError::InternalError(format!("Failed to register builtins: {}", e)))?;

    // ... 后续代码不变
}
```

---

## 六、实施步骤

1. **`Cargo.toml`** — 添加 7 个新依赖
2. **`src/sandbox/js_builtins.rs`** — 创建文件，实现 `register_all` + 5 组 API 注册函数 + 所有原生函数
3. **`src/sandbox/mod.rs`** — 添加 `pub mod js_builtins;`
4. **`src/sandbox/builtin.rs`** — 在 `execute_js` 中调用 `register_all`
5. **`src/sandbox/js_builtins.rs` 单元测试** — 为每个 API 写测试
6. **`src/sandbox/builtin.rs` 集成测试** — 通过完整沙箱流程测试内置 API
7. **`cargo test`** — 确保所有测试通过

---

## 七、测试用例

在 `src/sandbox/builtin.rs` 的 `#[cfg(test)] mod tests` 中添加集成测试：

| 测试名 | 验证点 |
|--------|--------|
| `test_builtin_uuidv4` | `uuidv4()` 返回合法 UUID 格式 |
| `test_builtin_btoa_atob` | `atob(btoa("hello"))` 往返一致 |
| `test_builtin_datetime_now` | `datetime.now()` 返回合理时间戳 |
| `test_builtin_datetime_format` | `datetime.format()` 返回日期字符串 |
| `test_builtin_crypto_md5` | `crypto.md5("hello")` 结果与已知值对比 |
| `test_builtin_crypto_sha256` | `crypto.sha256("hello")` 结果与已知值对比 |
| `test_builtin_crypto_hmac` | `crypto.hmacSha256(key, data)` 结果验证 |
| `test_builtin_crypto_aes` | `aesDecrypt(aesEncrypt(text, key), key)` 往返一致 |
| `test_builtin_random_int` | `randomInt(1, 10)` 返回 [1,10] 内的整数 |
| `test_builtin_random_bytes` | `randomBytes(16)` 返回 32 字符 hex |

---

## 八、验证方式

```bash
# 全量测试
cargo test

# 仅沙箱测试
cargo test sandbox

# 仅内置 API 测试
cargo test test_builtin_
```
