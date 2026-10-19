//! Retrying flaky network calls.

use std::{future::Future, time::Duration};

use crate::Result;

/// Runs `op` up to `attempts` times, waiting 0.5 s, 1 s, 2 s, … between tries
/// (exponential backoff), and returns the first success or the last error.
///
/// Busy free services (archive.org, Google Drive) answer the odd 429/503;
/// one or two calm retries usually get through.
pub async fn with_backoff<T, F, Fut>(attempts: u32, mut op: F) -> Result<T>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T>>,
{
    let mut wait = Duration::from_millis(500);
    for _ in 1..attempts {
        if let Ok(value) = op().await {
            return Ok(value);
        }
        tokio::time::sleep(wait).await;
        wait *= 2;
    }
    op().await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[tokio::test]
    async fn retries_until_success() {
        let calls = Cell::new(0);
        let got = with_backoff(3, || {
            calls.set(calls.get() + 1);
            let n = calls.get();
            async move { if n < 2 { Err("busy".into()) } else { Ok(n) } }
        })
        .await;
        assert_eq!(got.unwrap(), 2);
        assert_eq!(calls.get(), 2);
    }

    #[tokio::test]
    async fn gives_up_with_the_last_error() {
        let calls = Cell::new(0);
        let got: Result<()> = with_backoff(1, || {
            calls.set(calls.get() + 1);
            async { Err("down".into()) }
        })
        .await;
        assert_eq!(got.unwrap_err().to_string(), "down");
        assert_eq!(calls.get(), 1, "one attempt means no retry");
    }
}
