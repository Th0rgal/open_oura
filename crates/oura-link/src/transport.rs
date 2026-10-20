//! Transport abstraction over the ring's BLE link.
//!
//! The protocol is request/response with asynchronous notifications. [`Transport`]
//! captures just what the client needs — write a request, and subscribe to the
//! stream of inbound frames — so the higher layers can be exercised with a mock
//! in tests while [`crate::ble`] provides the real `btleplug` implementation.

use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::broadcast;

use crate::error::Result;

/// Timing lines from `transact_until`, drained by the host app into its diagnostics log.
static DIAG: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

/// Take (and clear) the timing lines recorded since the last call.
pub fn take_diag() -> Vec<String> {
    std::mem::take(&mut *DIAG.lock().unwrap())
}

fn push_diag(line: String) {
    let mut d = DIAG.lock().unwrap();
    if d.len() < 256 {
        d.push(line);
    }
}

/// A bidirectional link to a ring.
#[async_trait]
pub trait Transport: Send + Sync {
    /// Write a raw request frame to the ring's write characteristic.
    async fn write(&self, data: &[u8]) -> Result<()>;

    /// Subscribe to inbound notification frames (raw bytes, one per notification).
    fn subscribe(&self) -> broadcast::Receiver<Vec<u8>>;
}

/// Write `request` and collect notification frames until the link is quiet for
/// `quiet` (i.e. no new frame arrives within that window). This matches the
/// ring's behaviour of emitting one or more notifications per request with no
/// explicit terminator on most commands.
pub async fn transact<T>(transport: &T, request: &[u8], quiet: Duration) -> Result<Vec<Vec<u8>>>
where
    T: Transport + ?Sized,
{
    transact_until(transport, request, quiet, |_| false).await
}

/// Like [`transact`], but returns as soon as a frame satisfying `is_terminal`
/// arrives, instead of always waiting out the quiet window after the last frame.
///
/// Most ring commands have a well-known response (the official app proceeds the
/// moment it sees it), so terminating on it saves the full `quiet` window per
/// request — which dominates the handshake/setup phase otherwise. The quiet
/// window remains as the fallback for unexpected responses or a dead link.
pub async fn transact_until<T, F>(
    transport: &T,
    request: &[u8],
    quiet: Duration,
    mut is_terminal: F,
) -> Result<Vec<Vec<u8>>>
where
    T: Transport + ?Sized,
    F: FnMut(&[u8]) -> bool,
{
    let mut rx = transport.subscribe();
    // Drop any backlog so we only observe responses to *this* request.
    while rx.try_recv().is_ok() {}

    let began = std::time::Instant::now();
    transport.write(request).await?;
    let written_ms = began.elapsed().as_millis();

    let mut frames = Vec::new();
    let mut first_ms = None;
    let mut bytes = 0usize;
    loop {
        match tokio::time::timeout(quiet, rx.recv()).await {
            Ok(Ok(frame)) => {
                first_ms.get_or_insert_with(|| began.elapsed().as_millis());
                bytes += frame.len();
                let done = is_terminal(&frame);
                frames.push(frame);
                if done {
                    break;
                }
            }
            Ok(Err(broadcast::error::RecvError::Lagged(_))) => continue,
            // Channel closed or quiet window elapsed: we're done collecting.
            _ => break,
        }
    }
    push_diag(format!(
        "req 0x{:02x} written={}ms first={}ms total={}ms frames={} bytes={}",
        request.first().copied().unwrap_or(0),
        written_ms,
        first_ms.map_or(-1, |m| m as i64),
        began.elapsed().as_millis(),
        frames.len(),
        bytes
    ));
    Ok(frames)
}

#[cfg(test)]
pub(crate) mod mock {
    //! A scripted transport for unit tests: maps request hex prefixes to canned
    //! response frames.
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    pub struct MockTransport {
        tx: broadcast::Sender<Vec<u8>>,
        responses: Mutex<HashMap<String, Vec<Vec<u8>>>>,
        writes: Mutex<Vec<Vec<u8>>>,
    }

    impl MockTransport {
        pub fn new() -> Self {
            let (tx, _) = broadcast::channel(64);
            Self {
                tx,
                responses: Mutex::new(HashMap::new()),
                writes: Mutex::new(Vec::new()),
            }
        }

        /// Register canned responses keyed by the request's full hex.
        pub fn on(&self, request_hex: &str, responses: &[&str]) {
            self.responses.lock().unwrap().insert(
                request_hex.to_string(),
                responses.iter().map(|h| hex::decode(h).unwrap()).collect(),
            );
        }

        pub fn writes(&self) -> Vec<Vec<u8>> {
            self.writes.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl Transport for MockTransport {
        async fn write(&self, data: &[u8]) -> Result<()> {
            self.writes.lock().unwrap().push(data.to_vec());
            let key = hex::encode(data);
            if let Some(frames) = self.responses.lock().unwrap().get(&key) {
                for f in frames {
                    let _ = self.tx.send(f.clone());
                }
            }
            Ok(())
        }

        fn subscribe(&self) -> broadcast::Receiver<Vec<u8>> {
            self.tx.subscribe()
        }
    }
}
