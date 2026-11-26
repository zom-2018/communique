use std::future::Future;
use std::time::{Duration, SystemTime};

use crate::error::Result;

const MAX_RETRIES: u32 = 10;
const INITIAL_DELAY_MS: u64 = 1_000;
const MAX_DELAY_MS: u64 = 60_000;

pub struct RetryConfig {
    pub max_retries: u32,
    pub initial_delay_ms: u64,
    pub max_delay_ms: u64,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_retries: MAX_RETRIES,
            initial_delay_ms: INITIAL_DELAY_MS,
            max_delay_ms: MAX_DELAY_MS,
        }
    }
}

fn is_retryable_status(status: reqwest::StatusCode) -> bool {
    matches!(status.as_u16(), 429 | 500 | 502 | 503 | 529)
}

fn jitter_ms() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos() as u64
        % 500
}

pub async fn retry_request<F, Fut>(context: &str, request_fn: F) -> Result<reqwest::Response>
where
    F: Fn() -> Fut + Send,
    Fut: Future<Output = std::result::Result<reqwest::Response, reqwest::Error>> + Send,
{
    let config = RetryConfig::default();
    retry_request_with_config(context, &config, request_fn).await
}

pub async fn retry_request_with_config<F, Fut>(
    context: &str,
    config: &RetryConfig,
    request_fn: F,
) -> Result<reqwest::Response>
where
    F: Fn() -> Fut + Send,
    Fut: Future<Output = std::result::Result<reqwest::Response, reqwest::Error>> + Send,
{
    let mut delay_ms = config.initial_delay_ms;

    for attempt in 0..=config.max_retries {
        let result = request_fn().await;

        match result {
            Ok(resp) if is_retryable_status(resp.status()) => {
                if attempt == config.max_retries {
                    return Ok(resp);
                }

                let wait = if resp.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
                    resp.headers()
                        .get("retry-after")
                        .and_then(|v| v.to_str().ok())
                        .and_then(|s| s.parse::<u64>().ok())
                        .map(|secs| secs.saturating_mul(1000).min(config.max_delay_ms))
                        .unwrap_or(delay_ms)
                } else {
                    delay_ms
                };

                let jitter = jitter_ms();
                log::warn!(
                    "{context}: {} (attempt {}/{}), retrying in {}ms",
                    resp.status(),
                    attempt + 1,
                    config.max_retries + 1,
                    wait + jitter,
                );
                tokio::time::sleep(Duration::from_millis(wait + jitter)).await;
                delay_ms = (delay_ms * 2).min(config.max_delay_ms);
            }
            Ok(resp) => return Ok(resp),
            Err(e) if e.is_connect() || e.is_timeout() => {
                if attempt == config.max_retries {
                    return Err(e.without_url().into());
                }
                let jitter = jitter_ms();
                log::warn!(
                    "{context}: {} (attempt {}/{}), retrying in {}ms",
                    e.without_url(),
                    attempt + 1,
                    config.max_retries + 1,
                    delay_ms + jitter,
                );
                tokio::time::sleep(Duration::from_millis(delay_ms + jitter)).await;
                delay_ms = (delay_ms * 2).min(config.max_delay_ms);
            }
            Err(e) => return Err(e.without_url().into()),
        }
    }

    unreachable!()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use wiremock::matchers::method;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn fast_config() -> RetryConfig {
        RetryConfig {
            max_retries: 3,
            initial_delay_ms: 1,
            max_delay_ms: 10,
        }
    }

    /// A responder that returns responses in order, repeating the last one if exhausted.
    struct Sequence {
        call_count: AtomicUsize,
        statuses: Vec<u16>,
        headers: Vec<Vec<(&'static str, &'static str)>>,
    }

    impl Sequence {
        fn status(statuses: Vec<u16>) -> Self {
            let headers = vec![vec![]; statuses.len()];
            Self {
                call_count: AtomicUsize::new(0),
                statuses,
                headers,
            }
        }

        fn with_header(mut self, index: usize, name: &'static str, value: &'static str) -> Self {
            self.headers[index].push((name, value));
            self
        }
    }

    impl wiremock::Respond for Sequence {
        fn respond(&self, _: &wiremock::Request) -> ResponseTemplate {
            let idx = self
                .call_count
                .fetch_add(1, Ordering::SeqCst)
                .min(self.statuses.len() - 1);
            let mut resp = ResponseTemplate::new(self.statuses[idx]);
            for &(name, value) in &self.headers[idx] {
                resp = resp.insert_header(name, value);
            }
            resp
        }
    }

    async fn mock_sequence(sequence: Sequence, expected: u64) -> MockServer {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(sequence)
            .expect(expected)
            .mount(&server)
            .await;
        server
    }

    async fn request(server: &MockServer, config: &RetryConfig) -> reqwest::Response {
        let client = reqwest::Client::new();
        let url = server.uri();
        retry_request_with_config("test", config, || client.get(&url).send())
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn test_success_no_retry() {
        let server = mock_sequence(Sequence::status(vec![200]), 1).await;

        let resp = request(&server, &fast_config()).await;
        assert_eq!(resp.status(), 200);
    }

    #[tokio::test]
    async fn test_retry_on_500_then_success() {
        let server = mock_sequence(Sequence::status(vec![500, 200]), 2).await;

        let resp = request(&server, &fast_config()).await;
        assert_eq!(resp.status(), 200);
    }

    #[tokio::test]
    async fn test_no_retry_on_400() {
        let server = mock_sequence(Sequence::status(vec![400]), 1).await;

        let resp = request(&server, &fast_config()).await;
        assert_eq!(resp.status(), 400);
    }

    #[tokio::test]
    async fn test_retry_on_429() {
        let server = mock_sequence(Sequence::status(vec![429, 200]), 2).await;

        let resp = request(&server, &fast_config()).await;
        assert_eq!(resp.status(), 200);
    }

    #[test]
    fn test_retryable_statuses() {
        use reqwest::StatusCode;
        assert!(is_retryable_status(StatusCode::TOO_MANY_REQUESTS));
        assert!(is_retryable_status(StatusCode::INTERNAL_SERVER_ERROR));
        assert!(is_retryable_status(StatusCode::BAD_GATEWAY));
        assert!(is_retryable_status(StatusCode::SERVICE_UNAVAILABLE));
        assert!(is_retryable_status(StatusCode::from_u16(529).unwrap()));
        assert!(!is_retryable_status(StatusCode::OK));
        assert!(!is_retryable_status(StatusCode::BAD_REQUEST));
        assert!(!is_retryable_status(StatusCode::UNAUTHORIZED));
        assert!(!is_retryable_status(StatusCode::FORBIDDEN));
        assert!(!is_retryable_status(StatusCode::NOT_FOUND));
    }

    #[tokio::test]
    async fn gitlab_retry_after_is_bounded_even_for_extreme_values() {
        let server = mock_sequence(
            Sequence::status(vec![429, 200]).with_header(0, "retry-after", "18446744073709551615"),
            2,
        )
        .await;
        let response =
            tokio::time::timeout(Duration::from_secs(2), request(&server, &fast_config()))
                .await
                .expect("retry must be bounded");
        assert_eq!(response.status(), 200);
    }

    #[tokio::test]
    async fn test_retry_after_header() {
        let server = mock_sequence(
            Sequence::status(vec![429, 200]).with_header(0, "retry-after", "1"),
            2,
        )
        .await;

        let start = std::time::Instant::now();
        let config = RetryConfig {
            max_delay_ms: 2_000,
            ..fast_config()
        };
        let resp = request(&server, &config).await;
        let elapsed = start.elapsed();
        assert_eq!(resp.status(), 200);
        assert!(
            elapsed >= Duration::from_secs(1),
            "expected >=1s, got {elapsed:?}"
        );
    }
}
