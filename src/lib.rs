use log::{debug, info, warn};
use std::future::Future;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct RetryConfig {
    /// 最大重试次数
    pub max_attempts: u32,
    /// 最小重试延迟时间（毫秒）
    pub min_delay_ms: u64,
    /// 最大重试延迟时间（毫秒）
    pub max_delay_ms: u64,
    /// 重试抖动系数，0.0 = 无抖动，1.0 = 最大抖动
    /// 重试延迟时间 = 基础延迟时间 * (1 + 随机数 * 抖动系数)
    pub jitter: f64,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            min_delay_ms: 300,
            max_delay_ms: 30_000,
            jitter: 0.2,
        }
    }
}

#[derive(Debug)]
pub enum RetryResult<T, E> {
    /// 重试成功，包含结果和尝试次数（包括成功的那次）
    Success { result: T, attempts: u32 },
    /// 重试失败，且已超过最大重试次数
    Exhausted { last_error: E, attempts: u32 },
}

/// Compute the delay for a given attempt (0-indexed).
fn compute_backoff(config: &RetryConfig, attempt: u32) -> u64 {
    let base = config
        .min_delay_ms
        .saturating_mul(1u64.checked_shl(attempt).unwrap_or(u64::MAX));
    let capped = base.min(config.max_delay_ms);

    if config.jitter <= 0.0 {
        return capped;
    }

    let frac = pseudo_random_fraction();
    let jitter_offset = (capped as f64) * frac * config.jitter;
    let with_jitter = (capped as f64) + jitter_offset;

    (with_jitter as u64).min(config.max_delay_ms)
}

fn pseudo_random_fraction() -> f64 {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos();
    let mixed = nanos.wrapping_mul(2654435761);
    (mixed as f64) / (u32::MAX as f64)
}

/// - `config`：重试配置，包含最大尝试次数、等待时间和抖动系数。
/// - `action`：要执行的异步闭包。每次尝试时都会调用一次。
/// - `should_retry`：根据错误判断继续重试。`true` 表示继续重试，`false` 停止
/// - `retry_after_hint`：根据错误判断，重试等待时间，
/// 返回 Some(ms) 则优先使用该等待时间，
/// 返回 None 计算退避时间；但最终仍会受 `max_delay_ms` 限制
pub async fn retry_async<F, Fut, T, E, P, H>(
    config: &RetryConfig,
    mut action: F,
    should_retry: P,
    retry_after_hint: H,
) -> RetryResult<T, E>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, E>>,
    P: Fn(&E) -> bool,
    H: Fn(&E) -> Option<u64>,
    E: std::fmt::Debug,
{
    let max = config.max_attempts.max(1);
    let mut last_error: Option<E> = None;

    for attempt in 0..max {
        match action().await {
            Ok(result) => {
                if attempt > 0 {
                    info!("retry succeeded after {} previous failures", attempt);
                }
                return RetryResult::Success {
                    result,
                    attempts: attempt + 1,
                };
            }
            Err(err) => {
                let is_last = attempt + 1 >= max;

                if is_last || !should_retry(&err) {
                    if !should_retry(&err) {
                        debug!(
                            "error is not retryable,{}, giving up: {:?}",
                            attempt + 1,
                            err
                        );
                    } else {
                        warn!(
                            "all retry attempts failed: {:?}, attempt + 1={},max_attempts {}",
                            err,
                            attempt + 1,
                            max,
                        );
                    }
                    return RetryResult::Exhausted {
                        last_error: err,
                        attempts: attempt + 1,
                    };
                }

                let hint = retry_after_hint(&err);
                let delay_ms = if let Some(hinted) = hint {
                    hinted.min(config.max_delay_ms)
                } else {
                    compute_backoff(config, attempt)
                };

                debug!(
                    "attempt {}, delay_ms {} ms retrying after error: {:?}",
                    attempt + 1,
                    delay_ms,
                    err
                );

                tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                last_error = Some(err);
            }
        }
    }

    RetryResult::Exhausted {
        last_error: last_error.expect("at least one attempt should have been made"),
        attempts: max,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};
    pub fn network_retry_config() -> RetryConfig {
        RetryConfig {
            max_attempts: 3,
            min_delay_ms: 500,
            max_delay_ms: 30_000,
            jitter: 0.1,
        }
    }

    #[test]
    fn test_compute_backoff_capped() {
        let config = RetryConfig {
            max_attempts: 10,
            min_delay_ms: 1_000,
            max_delay_ms: 5_000,
            jitter: 0.0,
        };

        // 1000 * 2^0 = 1000
        assert_eq!(compute_backoff(&config, 0), 1_000);
        // 1000 * 2^1 = 2000
        assert_eq!(compute_backoff(&config, 1), 2_000);
        // 1000 * 2^2 = 4000
        assert_eq!(compute_backoff(&config, 2), 4_000);
        // 1000 * 2^3 = 8000, capped at 5000
        assert_eq!(compute_backoff(&config, 3), 5_000);
        // Further attempts stay capped
        assert_eq!(compute_backoff(&config, 10), 5_000);
    }

    #[tokio::test]
    async fn test_retry_success_first_try() {
        let config = RetryConfig {
            max_attempts: 3,
            min_delay_ms: 10,
            max_delay_ms: 100,
            jitter: 0.0,
        };

        let outcome = retry_async(
            &config,
            || async { Ok::<&str, &str>("hello") },
            |_| true,
            |_: &&str| None,
        )
        .await;

        match outcome {
            RetryResult::Success { result, attempts } => {
                assert_eq!(result, "hello");
                assert_eq!(attempts, 1);
            }
            _ => panic!("expected success"),
        }
    }

    #[tokio::test]
    async fn test_retry_success_after_failures() {
        let config = RetryConfig {
            max_attempts: 5,
            min_delay_ms: 1, // tiny delays for test speed
            max_delay_ms: 10,
            jitter: 0.0,
        };

        let counter = Arc::new(AtomicU32::new(0));
        let counter_clone = counter.clone();

        let outcome = retry_async(
            &config,
            move || {
                let c = counter_clone.clone();
                async move {
                    let n = c.fetch_add(1, Ordering::SeqCst);
                    if n < 2 { Err("not yet") } else { Ok("finally") }
                }
            },
            |_| true,
            |_: &&str| None,
        )
        .await;

        match outcome {
            RetryResult::Success { result, attempts } => {
                assert_eq!(result, "finally");
                assert_eq!(attempts, 3); // failed twice, succeeded on 3rd
            }
            _ => panic!("expected success"),
        }
    }

    #[tokio::test]
    async fn test_retry_exhausted() {
        let config = RetryConfig {
            max_attempts: 3,
            min_delay_ms: 1,
            max_delay_ms: 10,
            jitter: 0.0,
        };

        let outcome = retry_async(
            &config,
            || async { Err::<(), &str>("always fails") },
            |_| true,
            |_: &&str| None,
        )
        .await;

        match outcome {
            RetryResult::Exhausted {
                last_error,
                attempts,
            } => {
                println!("{last_error}");
                println!("{attempts}");
            }
            _ => panic!("expected exhausted"),
        }
    }

    #[tokio::test]
    async fn test_retry_non_retryable_error() {
        let config = RetryConfig {
            max_attempts: 5,
            min_delay_ms: 1,
            max_delay_ms: 10,
            jitter: 0.0,
        };

        let counter = Arc::new(AtomicU32::new(0));
        let counter_clone = counter.clone();

        let outcome = retry_async(
            &config,
            move || {
                let c = counter_clone.clone();
                async move {
                    c.fetch_add(1, Ordering::SeqCst);
                    Err::<(), &str>("fatal error")
                }
            },
            |_| false, // never retry
            |_: &&str| None,
        )
        .await;

        match outcome {
            RetryResult::Exhausted {
                last_error,
                attempts,
            } => {
                assert_eq!(last_error, "fatal error");
                assert_eq!(attempts, 1); // gave up immediately
            }
            _ => panic!("expected exhausted"),
        }

        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn test_retry_with_hint_delay() {
        let config = RetryConfig {
            max_attempts: 3,
            min_delay_ms: 10_000, // large base delay
            max_delay_ms: 60_000,
            jitter: 0.0,
        };

        let counter = Arc::new(AtomicU32::new(0));
        let counter_clone = counter.clone();

        let start = std::time::Instant::now();

        let outcome = retry_async(
            &config,
            move || {
                let c = counter_clone.clone();
                async move {
                    let n = c.fetch_add(1, Ordering::SeqCst);
                    if n < 1 { Err("transient") } else { Ok("ok") }
                }
            },
            |_| true,
            |_: &&str| Some(1), // hint: 1ms delay (overrides 10s base)
        )
        .await;

        let elapsed = start.elapsed();

        match outcome {
            RetryResult::Success { result, attempts } => {
                assert_eq!(result, "ok");
                assert_eq!(attempts, 2);
                // Should complete in well under 1 second (hint was 1ms,
                // not the 10s base delay).
                assert!(
                    elapsed.as_millis() < 5_000,
                    "retry took too long: {:?} 鈥?hint should have overridden base delay",
                    elapsed
                );
            }
            _ => panic!("expected success"),
        }
    }

    #[test]
    fn test_network_retry_config() {
        let config = network_retry_config();
        assert_eq!(config.max_attempts, 3);
        assert_eq!(config.min_delay_ms, 500);
        assert_eq!(config.max_delay_ms, 30_000);
        assert!((config.jitter - 0.1).abs() < f64::EPSILON);
    }
}
