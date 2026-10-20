//! Staying within Google's per-user rate limits. Every request waits its turn at a steady
//! pace, the pace slows when Google pushes back and recovers gradually, and rate-limited or
//! briefly failing requests are retried with exponential backoff and jitter.

use std::time::Duration;

use tokio::sync::Mutex;
use tokio::time::Instant;

use crate::api::DriveApi;
use crate::{Error, Result};

/// About eight requests a second: well under Google's default per-user quota, with room
/// for the app's other processes.
const BASE_INTERVAL: Duration = Duration::from_millis(125);
const SLOWEST_INTERVAL: Duration = Duration::from_secs(2);
const ATTEMPTS: u32 = 6;
const FIRST_BACKOFF: Duration = Duration::from_secs(1);
const LONGEST_BACKOFF: Duration = Duration::from_secs(64);

pub(crate) struct Pacer {
    state: Mutex<(Instant, Duration)>,
}

impl Default for Pacer {
    fn default() -> Self {
        Self {
            state: Mutex::new((Instant::now(), BASE_INTERVAL)),
        }
    }
}

impl Pacer {
    /// Waits for this request's slot.
    async fn wait(&self) {
        let slot = {
            let mut state = self.state.lock().await;
            let slot = state.0.max(Instant::now());
            state.0 = slot + state.1;
            slot
        };
        tokio::time::sleep_until(slot).await;
    }

    /// Google pushed back: halve the pace.
    async fn slow_down(&self) {
        let mut state = self.state.lock().await;
        state.1 = (state.1 * 2).min(SLOWEST_INTERVAL);
        tracing::info!(
            interval_ms = state.1.as_millis() as u64,
            "slowing Drive requests"
        );
    }

    /// A request went through: recover a little of the pace.
    async fn recover(&self) {
        let mut state = self.state.lock().await;
        if state.1 > BASE_INTERVAL {
            state.1 = (state.1 * 9 / 10).max(BASE_INTERVAL);
        }
    }
}

/// Why a response should be retried, if it should.
enum Retry {
    RateLimited,
    ServerError,
}

impl DriveApi {
    /// Sends a request at a pace Google allows. Rate limits and transient server errors
    /// are retried with backoff (honoring `Retry-After`); a rate limit that persists
    /// becomes [`Error::RateLimited`]. Other responses, successful or not, are returned.
    pub(crate) async fn send(&self, request: reqwest::RequestBuilder) -> Result<reqwest::Response> {
        // Only reads are safe to repeat after a server error: a write may have landed.
        let idempotent = request
            .try_clone()
            .and_then(|r| r.build().ok())
            .is_some_and(|r| r.method() == reqwest::Method::GET);
        let mut backoff = FIRST_BACKOFF;
        for attempt in 0..ATTEMPTS {
            self.pacer.wait().await;
            let attempt_request = request
                .try_clone()
                .expect("Drive requests have bodies that can be resent");
            let response = attempt_request.send().await?;
            let status = response.status().as_u16();
            let retry_after = response
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.parse::<u64>().ok())
                .map(Duration::from_secs);

            let retry = match status {
                429 => Retry::RateLimited,
                500 | 502 | 503 | 504 if idempotent => Retry::ServerError,
                403 => {
                    // Drive reports most rate limits as 403 with a reason; other 403s are
                    // real permission errors and go back to the caller as errors.
                    let body: serde_json::Value = response.json().await.unwrap_or_default();
                    let reason = body["error"]["errors"][0]["reason"].as_str().unwrap_or("");
                    if is_rate_limit_reason(reason) {
                        Retry::RateLimited
                    } else {
                        let message = body["error"]["message"]
                            .as_str()
                            .unwrap_or("Forbidden")
                            .to_owned();
                        return Err(Error::Api {
                            status: 403,
                            message,
                        });
                    }
                }
                _ => {
                    self.pacer.recover().await;
                    return Ok(response);
                }
            };

            if matches!(retry, Retry::RateLimited) {
                self.pacer.slow_down().await;
            }
            if attempt + 1 == ATTEMPTS {
                return Err(match retry {
                    Retry::RateLimited => Error::RateLimited,
                    Retry::ServerError => Error::Api {
                        status,
                        message: "Google Drive is having trouble; try again soon".into(),
                    },
                });
            }
            let wait = retry_after.unwrap_or(backoff) + jitter(backoff);
            tracing::info!(
                status,
                wait_ms = wait.as_millis() as u64,
                "retrying a Drive request"
            );
            tokio::time::sleep(wait).await;
            backoff = (backoff * 2).min(LONGEST_BACKOFF);
        }
        unreachable!("the loop returns on its last attempt")
    }

    /// Sends a request at the shared pace without retrying it, for upload chunks, whose
    /// retries must first ask Drive how much it already holds.
    pub(crate) async fn send_paced(
        &self,
        request: reqwest::RequestBuilder,
    ) -> Result<reqwest::Response> {
        self.pacer.wait().await;
        let response = request.send().await?;
        match response.status().as_u16() {
            429 => self.pacer.slow_down().await,
            status if status < 400 => self.pacer.recover().await,
            _ => {}
        }
        Ok(response)
    }
}

pub(crate) fn is_rate_limit_reason(reason: &str) -> bool {
    matches!(
        reason,
        "userRateLimitExceeded" | "rateLimitExceeded" | "sharingRateLimitExceeded"
    )
}

/// Up to half the backoff again, so many clients don't retry in lockstep.
fn jitter(backoff: Duration) -> Duration {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos());
    backoff.mul_f64(f64::from(nanos % 1000) / 2000.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_rate_limit_reasons() {
        assert!(is_rate_limit_reason("userRateLimitExceeded"));
        assert!(is_rate_limit_reason("rateLimitExceeded"));
        assert!(!is_rate_limit_reason("insufficientFilePermissions"));
    }

    #[test]
    fn jitter_stays_within_half_the_backoff() {
        let backoff = Duration::from_secs(4);
        assert!(jitter(backoff) <= backoff / 2);
    }

    #[tokio::test(start_paused = true)]
    async fn paces_requests_and_slows_when_told() {
        let pacer = Pacer::default();
        let start = Instant::now();
        for _ in 0..4 {
            pacer.wait().await;
        }
        assert_eq!(start.elapsed(), BASE_INTERVAL * 3);

        pacer.slow_down().await;
        let before = Instant::now();
        pacer.wait().await;
        pacer.wait().await;
        assert!(before.elapsed() >= BASE_INTERVAL * 2);
    }
}
