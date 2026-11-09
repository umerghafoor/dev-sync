//! Exponential backoff with jitter for transient failures.

use crate::TransportError;
use std::time::Duration;

#[derive(Debug, Clone, Copy)]
pub struct RetryPolicy {
    /// 0 = retry forever.
    pub max_attempts: u32,
    pub base: Duration,
    pub max: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self { max_attempts: 0, base: Duration::from_millis(500), max: Duration::from_secs(30) }
    }
}

impl RetryPolicy {
    pub fn delay(&self, attempt: u32) -> Duration {
        let exp = self.base.saturating_mul(1u32 << attempt.min(16));
        let capped = exp.min(self.max);
        // +-20% jitter so many clients do not retry in lockstep.
        let nanos =
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.subsec_nanos()).unwrap_or(0);
        let factor = 0.8 + (nanos % 1000) as f64 / 2500.0;
        capped.mul_f64(factor)
    }
}

/// Errors that know whether a retry can help.
pub trait Transient {
    fn is_transient(&self) -> bool;
}

impl Transient for TransportError {
    fn is_transient(&self) -> bool {
        TransportError::is_transient(self)
    }
}

/// Runs `op` until it succeeds, fails with a non-transient error, or the
/// policy's attempts are exhausted. `on_retry(attempt, error, delay)` is
/// called before each sleep.
pub fn retry<T, E, F, R>(policy: &RetryPolicy, mut on_retry: R, mut op: F) -> Result<T, E>
where
    E: Transient,
    F: FnMut() -> Result<T, E>,
    R: FnMut(u32, &E, Duration),
{
    let mut attempt = 0u32;
    loop {
        match op() {
            Ok(v) => return Ok(v),
            Err(e) => {
                let transient = e.is_transient();
                attempt += 1;
                if !transient || (policy.max_attempts != 0 && attempt >= policy.max_attempts) {
                    return Err(e);
                }
                let d = policy.delay(attempt - 1);
                on_retry(attempt, &e, d);
                std::thread::sleep(d);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retries_transient_then_succeeds() {
        let p = RetryPolicy { max_attempts: 5, base: Duration::from_millis(1), max: Duration::from_millis(2) };
        let mut n = 0;
        let r: Result<u32, TransportError> = retry(
            &p,
            |_, _, _| {},
            || {
                n += 1;
                if n < 3 {
                    Err(TransportError::Connect("down".into()))
                } else {
                    Ok(n)
                }
            },
        );
        assert_eq!(r.unwrap(), 3);
    }

    #[test]
    fn stops_on_fatal_and_on_limit() {
        let p = RetryPolicy { max_attempts: 3, base: Duration::from_millis(1), max: Duration::from_millis(1) };
        let mut n = 0;
        let r: Result<(), TransportError> = retry(
            &p,
            |_, _, _| {},
            || {
                n += 1;
                Err(TransportError::Fatal("auth".into()))
            },
        );
        assert!(r.is_err());
        assert_eq!(n, 1);
        n = 0;
        let r: Result<(), TransportError> = retry(
            &p,
            |_, _, _| {},
            || {
                n += 1;
                Err(TransportError::Timeout(Duration::from_secs(1)))
            },
        );
        assert!(r.is_err());
        assert_eq!(n, 3);
    }

    #[test]
    fn delay_is_capped() {
        let p = RetryPolicy::default();
        assert!(p.delay(30) <= Duration::from_secs(36));
    }
}
