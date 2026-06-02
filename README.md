# retry-rs

`retry-rs` 是一个轻量的 Rust 异步重试工具，用于为容易出现临时失败的操作添加重试逻辑，例如网络请求、远程服务调用、限流后的再次尝试等。

它基于 Tokio 计时器工作，支持指数退避、自定义重试判断、最大延迟限制、抖动系数以及按错误返回的延迟提示。

## 功能特性

- 异步重试：通过 `retry_async` 包装任意返回 `Future<Output = Result<T, E>>` 的操作。
- 指数退避：延迟从 `min_delay_ms` 开始按 `2^n` 增长。
- 最大延迟限制：所有自动计算或提示的等待时间都会被 `max_delay_ms` 限制。
- 抖动控制：通过 `jitter` 为退避时间增加随机偏移，降低并发重试时的集中冲击。
- 自定义重试条件：通过 `should_retry` 判断某个错误是否值得继续重试。
- 延迟提示：通过 `retry_after_hint` 支持类似 HTTP `Retry-After` 的按错误指定等待时间。
- 结果可观测：返回成功结果、最后一次错误以及实际尝试次数。

## 安装

在使用方项目中添加依赖：

```toml
[dependencies]
retry-rs = "0.1.0"
tokio = { version = "1", features = ["time", "macros", "rt-multi-thread"] }
```

代码中通过 crate 名 `retry_rs` 引入：

```rust
use retry_rs::{retry_async, RetryConfig, RetryResult};
```

## 快速开始

```rust
use retry_rs::{retry_async, RetryConfig, RetryResult};

#[tokio::main]
async fn main() {
    let config = RetryConfig {
        max_attempts: 3,
        min_delay_ms: 200,
        max_delay_ms: 5_000,
        jitter: 0.2,
    };

    let mut calls = 0;

    let outcome = retry_async(
        &config,
        || {
            calls += 1;
            let current = calls;

            async move {
                if current < 3 {
                    Err("temporary error".to_string())
                } else {
                    Ok("ok")
                }
            }
        },
        |err| err.contains("temporary"),
        |_err| None,
    )
    .await;

    match outcome {
        RetryResult::Success { result, attempts } => {
            println!("success: {result}, attempts: {attempts}");
        }
        RetryResult::Exhausted {
            last_error,
            attempts,
        } => {
            println!("failed after {attempts} attempts: {last_error:?}");
        }
    }
}
```

## API 说明

### `RetryConfig`

```rust
pub struct RetryConfig {
    pub max_attempts: u32,
    pub min_delay_ms: u64,
    pub max_delay_ms: u64,
    pub jitter: f64,
}
```

字段说明：

| 字段 | 默认值 | 说明 |
| --- | ---: | --- |
| `max_attempts` | `3` | 总尝试次数，包含第一次执行。即使设置为 `0`，内部也会至少尝试一次。 |
| `min_delay_ms` | `300` | 第一次重试前的基础等待时间，单位为毫秒。 |
| `max_delay_ms` | `30_000` | 单次等待时间上限，单位为毫秒。 |
| `jitter` | `0.2` | 抖动系数。`0.0` 表示不增加抖动。 |

### `retry_async`

```rust
pub async fn retry_async<F, Fut, T, E, P, H>(
    config: &RetryConfig,
    action: F,
    should_retry: P,
    retry_after_hint: H,
) -> RetryResult<T, E>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, E>>,
    P: Fn(&E) -> bool,
    H: Fn(&E) -> Option<u64>,
    E: std::fmt::Debug;
```

参数说明：

| 参数 | 说明 |
| --- | --- |
| `config` | 重试配置。 |
| `action` | 每次尝试都会执行的异步操作。 |
| `should_retry` | 根据错误判断是否继续重试。返回 `false` 时立即停止。 |
| `retry_after_hint` | 根据错误返回指定等待时间，单位为毫秒。返回 `Some(ms)` 时优先使用该值；返回 `None` 时使用指数退避计算。 |

### `RetryResult`

```rust
pub enum RetryResult<T, E> {
    Success { result: T, attempts: u32 },
    Exhausted { last_error: E, attempts: u32 },
}
```

- `Success`：操作成功，包含成功结果和实际尝试次数。
- `Exhausted`：操作失败，包含最后一次错误和实际尝试次数。

## 重试策略

当没有 `retry_after_hint` 时，等待时间按如下方式计算：

```text
delay = min(min_delay_ms * 2^attempt, max_delay_ms)
```

其中 `attempt` 从 `0` 开始计数。开启 `jitter` 后，会在基础延迟上增加一个随机偏移，最终仍不会超过 `max_delay_ms`。

如果 `retry_after_hint` 返回 `Some(ms)`，则实际等待时间为：

```text
delay = min(ms, max_delay_ms)
```

## 适用场景

- 网络请求的临时失败。
- 调用外部服务时的短暂不可用。
- 服务端返回限流或稍后重试提示。
- 数据库、队列或缓存服务的瞬时错误。

## 开发

运行测试：

```bash
cargo test
```

格式化代码：

```bash
cargo fmt
```

## License

MIT
