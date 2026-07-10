//! [`OuraClient`] — the high-level, transport-generic API.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::error::{Error, Result};
use crate::history::decode_batch;
use crate::transport::{transact_until, Transport};
use oura_protocol::auth::{encrypt_nonce, AuthResult};
use oura_protocol::device::{self, Battery, Capability, DeviceInfo};
use oura_protocol::events::RingEvent;
use oura_protocol::protocol::{self, feature, feature_mode, Packet};

/// Default quiet window for collecting responses to a request.
pub const DEFAULT_QUIET: Duration = Duration::from_millis(1500);

/// Human-readable one-line dump of parsed packets, for debug logs + error messages.
/// Shows each packet's tag, ext-tag and raw payload hex (payloads never carry the key).
fn dump_packets(packets: &[Packet]) -> String {
    if packets.is_empty() {
        return "<no packets — ring sent nothing before the quiet window>".to_string();
    }
    packets
        .iter()
        .map(|p| {
            format!(
                "{{tag=0x{:02x} ext={} payload={}}}",
                p.tag,
                p.ext_tag()
                    .map(|e| format!("0x{e:02x}"))
                    .unwrap_or_else(|| "-".to_string()),
                hex::encode(&p.payload)
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// One live heart-rate sample derived from an IBI subscription notification.
#[derive(Clone, Copy, Debug)]
pub struct HeartRateSample {
    pub bpm: u16,
    pub ibi_ms: u16,
}

/// One live accelerometer sample (signed raw counts) from the ACM stream.
#[derive(Clone, Copy, Debug)]
pub struct AcmSample {
    pub x: i16,
    pub y: i16,
    pub z: i16,
}

impl AcmSample {
    /// Vector magnitude, useful for motion/wave detection.
    pub fn magnitude(&self) -> f64 {
        ((self.x as f64).powi(2) + (self.y as f64).powi(2) + (self.z as f64).powi(2)).sqrt()
    }

    /// Parse an ACM measurement-indication frame (tag `0x33`) into its samples.
    pub fn parse_frame(frame: &[u8]) -> Vec<AcmSample> {
        parse_acm_frame(frame)
    }
}

/// Latest cached feature values read on demand (not a live stream).
#[derive(Clone, Copy, Debug, Default)]
pub struct LatestValues {
    /// Heart rate in bpm, if the feature reported one.
    pub bpm: Option<u16>,
    /// Blood-oxygen saturation in percent (SpO2 feature only).
    pub spo2_percent: Option<u8>,
}

/// Outcome of an event-drain sync.
#[derive(Clone, Copy, Debug)]
pub struct SyncOutcome {
    pub events_synced: u32,
    pub next_cursor: u32,
}

/// Progress after each fully-processed event batch: the checkpointed cursor,
/// the ring's own count of bytes still waiting, and events synced so far.
#[derive(Clone, Copy, Debug)]
pub struct BatchProgress {
    pub next_cursor: u32,
    pub bytes_left: u32,
    pub events_synced: u32,
}

/// Quiet-window fallback for event-batch requests. Batches terminate on the
/// ring's summary packet, so this only fires when the link died — but a ring can
/// legitimately pause mid-batch (bundling/flash reads), so it is deliberately
/// more patient than the per-command window to avoid false "link lost" errors.
const DRAIN_QUIET: Duration = Duration::from_secs(6);

/// Events requested per extended-drain batch. The official app asks for 65535
/// (everything) in one batch, but the cursor can only be checkpointed at batch
/// boundaries — one giant batch means a dropped link forfeits ALL progress and
/// gives no progress reporting. A few thousand events (~1 min of transfer) keeps
/// the per-batch round-trip overhead negligible while bounding what a drop can
/// lose and yielding regular `bytes_left` progress updates.
const EXT_BATCH_MAX_EVENTS: u16 = 4096;

/// A feature's reported status (`0x2f` ext `0x21`): mode/status/state/subscription.
#[derive(Clone, Copy, Debug)]
pub struct FeatureStatus {
    pub feature: u8,
    pub mode: u8,
    pub status: u8,
    pub state: u8,
    pub subscription: u8,
}

impl FeatureStatus {
    fn parse(p: &Packet) -> Option<FeatureStatus> {
        if p.ext_tag() != Some(0x21) || p.payload.len() < 6 {
            return None;
        }
        Some(FeatureStatus {
            feature: p.payload[1],
            mode: p.payload[2],
            status: p.payload[3],
            state: p.payload[4],
            subscription: p.payload[5],
        })
    }
}

/// High-level client over any [`Transport`].
pub struct OuraClient<T: Transport> {
    transport: T,
    quiet: Duration,
}

impl<T: Transport> OuraClient<T> {
    /// Wrap a transport with the default response window.
    pub fn new(transport: T) -> Self {
        Self {
            transport,
            quiet: DEFAULT_QUIET,
        }
    }

    /// Override the per-request quiet window.
    pub fn with_quiet(mut self, quiet: Duration) -> Self {
        self.quiet = quiet;
        self
    }

    /// Borrow the underlying transport (e.g. to disconnect a BLE link).
    pub fn transport(&self) -> &T {
        &self.transport
    }

    async fn request(&self, bytes: &[u8]) -> Result<Vec<Packet>> {
        self.request_until(bytes, |_| false).await
    }

    /// Request whose response ends with a known packet: returns as soon as a
    /// packet matching `terminal` arrives instead of waiting out the quiet
    /// window (the quiet window remains as fallback for errors / dead links).
    async fn request_until(
        &self,
        bytes: &[u8],
        terminal: impl Fn(&Packet) -> bool,
    ) -> Result<Vec<Packet>> {
        tracing::debug!(tx = %hex::encode(bytes), tx_len = bytes.len(), "→ request");
        let frames = transact_until(&self.transport, bytes, self.quiet, |frame| {
            Packet::parse_many(frame).iter().any(&terminal)
        })
        .await?;
        for f in &frames {
            tracing::debug!(rx = %hex::encode(f), rx_len = f.len(), "← frame");
        }
        let packets: Vec<Packet> = frames.iter().flat_map(|f| Packet::parse_many(f)).collect();
        tracing::debug!(packets = %dump_packets(&packets), "  parsed");
        Ok(packets)
    }

    /// Request that completes when a packet with `tag` arrives.
    async fn request_tag(&self, bytes: &[u8], tag: u8) -> Result<Vec<Packet>> {
        self.request_until(bytes, |p| p.tag == tag).await
    }

    /// Request that completes when an extended (`0x2f`) packet with `ext` arrives.
    async fn request_ext(&self, bytes: &[u8], ext: u8) -> Result<Vec<Packet>> {
        self.request_until(bytes, |p| p.ext_tag() == Some(ext))
            .await
    }

    /// Event-batch request: terminates on `terminal` like [`Self::request_until`],
    /// but with the more patient [`DRAIN_QUIET`] fallback (see its doc).
    async fn request_batch(
        &self,
        bytes: &[u8],
        terminal: impl Fn(&Packet) -> bool,
    ) -> Result<Vec<Packet>> {
        // 4× the configured window, capped at DRAIN_QUIET — so tests with a tiny
        // quiet stay fast while the default gets the patient 6 s fallback.
        let quiet = (self.quiet * 4).min(DRAIN_QUIET).max(self.quiet);
        tracing::debug!(tx = %hex::encode(bytes), tx_len = bytes.len(), "→ batch request");
        let frames = transact_until(&self.transport, bytes, quiet, |frame| {
            Packet::parse_many(frame).iter().any(&terminal)
        })
        .await?;
        let packets: Vec<Packet> = frames.iter().flat_map(|f| Packet::parse_many(f)).collect();
        tracing::debug!(packets = %dump_packets(&packets), "  parsed");
        Ok(packets)
    }

    fn find(packets: &[Packet], tag: u8) -> Option<&Packet> {
        packets.iter().find(|p| p.tag == tag)
    }

    // --- device info -------------------------------------------------------

    /// Read firmware/version metadata (no auth required).
    pub async fn firmware(&self) -> Result<DeviceInfo> {
        let packets = self.request_tag(&protocol::req_firmware(), 0x09).await?;
        Self::find(&packets, 0x09)
            .and_then(DeviceInfo::parse)
            .ok_or_else(|| Error::Protocol("no firmware response".into()))
    }

    /// Read battery state (requires app-auth on rings with a key installed).
    pub async fn battery(&self) -> Result<Battery> {
        let packets = self.request_tag(&protocol::req_battery(), 0x0d).await?;
        Self::find(&packets, 0x0d)
            .and_then(Battery::parse)
            .ok_or_else(|| Error::Protocol("no battery response (auth required?)".into()))
    }

    /// Read the ring serial number.
    pub async fn serial(&self) -> Result<String> {
        let packets = self.request_tag(&protocol::product::SERIAL, 0x19).await?;
        Self::find(&packets, 0x19)
            .and_then(device::parse_product_ascii)
            .ok_or_else(|| Error::Protocol("no serial response".into()))
    }

    /// Read the hardware id (e.g. `BLB_03`).
    pub async fn hardware_id(&self) -> Result<String> {
        let packets = self.request_tag(&protocol::product::HARDWARE, 0x19).await?;
        Self::find(&packets, 0x19)
            .and_then(device::parse_product_ascii)
            .ok_or_else(|| Error::Protocol("no hardware response".into()))
    }

    /// Read both capability pages.
    pub async fn capabilities(&self) -> Result<Vec<Capability>> {
        let mut caps = Vec::new();
        for page in 0u8..2 {
            let packets = self
                .request_ext(&protocol::req_capabilities(page), 0x02)
                .await?;
            if let Some(p) = packets.iter().find(|p| p.ext_tag() == Some(0x02)) {
                caps.extend(device::parse_capabilities(p));
            }
        }
        Ok(caps)
    }

    // --- auth & session ----------------------------------------------------

    /// Run the app-auth challenge with a 16-byte key. Must be repeated per
    /// connection on rings that have a key installed.
    pub async fn authenticate(&self, key: &[u8; 16]) -> Result<AuthResult> {
        // Never log key bytes (not even a slice — that leaks key material). Only the
        // length; "is it the right key" is answered by the Swift-side hashed fingerprint
        // and whether auth ultimately succeeds.
        tracing::debug!(
            key_len = key.len(),
            "auth: step 1 — requesting nonce (0x2b → expect 0x2c)"
        );
        let packets = self.request_ext(&protocol::req_auth_nonce(), 0x2c).await?;
        let nonce = match packets.iter().find(|p| p.ext_tag() == Some(0x2c)) {
            Some(p) if p.payload.len() > 1 => p.payload[1..].to_vec(),
            _ => {
                return Err(Error::Auth(format!(
                    "no nonce response (expected ext 0x2c). ring sent {} packet(s): [{}]",
                    packets.len(),
                    dump_packets(&packets)
                )));
            }
        };
        tracing::debug!(nonce = %hex::encode(&nonce), nonce_len = nonce.len(), "auth: step 2 — got nonce");

        // The encrypted block is deterministic from key+nonce via the shared Rust AES,
        // so it can't differ between clients and isn't logged (it's key-derived material).
        let encrypted = encrypt_nonce(key, &nonce);
        tracing::debug!("auth: step 3 — sending AES-128/ECB/PKCS7(nonce) (0x2d → expect 0x2e)");
        let packets = self
            .request_ext(&protocol::req_authenticate(&encrypted), 0x2e)
            .await?;
        let state = match packets
            .iter()
            .find(|p| p.ext_tag() == Some(0x2e))
            .and_then(|p| p.payload.get(1).copied())
        {
            Some(s) => s,
            None => {
                return Err(Error::Auth(format!(
                    "no authenticate response (expected ext 0x2e). ring sent {} packet(s): [{}]. \
                     nonce was {} ({}B)",
                    packets.len(),
                    dump_packets(&packets),
                    hex::encode(&nonce),
                    nonce.len()
                )));
            }
        };

        let result = AuthResult::from(state);
        tracing::debug!(state = %format!("0x{state:02x}"), ?result, "auth: step 4 — ring verdict");
        if result.is_success() {
            Ok(result)
        } else {
            // Rich, actionable failure: the exact state byte + what it means, plus the
            // bytes exchanged (never the key), so a report pins down key-vs-transport.
            let hint = match result {
                AuthResult::AuthenticationError => {
                    "the ring rejected the key — it does not match THIS ring's installed key \
                     (re-export the key from the phone that onboarded this exact ring)"
                }
                AuthResult::InFactoryReset => {
                    "the ring is factory-reset (no key installed yet) — pair/onboard it first"
                }
                AuthResult::NotOriginalOnboardedDevice => {
                    "the ring is bonded to a different onboarding — its key is not the one in use"
                }
                _ => "unexpected auth state",
            };
            Err(Error::Auth(format!(
                "ring rejected auth: state=0x{state:02x} ({result:?}) — {hint}. \
                 nonce={} ({}B)",
                hex::encode(&nonce),
                nonce.len()
            )))
        }
    }

    /// Install a new 16-byte auth key. Only valid on a factory-reset ring.
    pub async fn set_auth_key(&self, key: &[u8; 16]) -> Result<()> {
        let packets = self.request(&protocol::req_set_auth_key(key)).await?;
        match Self::find(&packets, 0x25).and_then(|p| p.payload.first().copied()) {
            Some(0x00) => Ok(()),
            Some(other) => Err(Error::Auth(format!("set_auth_key status {other:#04x}"))),
            None => Err(Error::Protocol("no set_auth_key response".into())),
        }
    }

    /// Align the ring clock to host UTC.
    pub async fn sync_time(&self) -> Result<()> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        self.request(&protocol::req_sync_time(now, 0)).await?;
        Ok(())
    }

    /// Align the ring clock using the official-app `12 09` counter form observed
    /// on Ring 4/5. Prefer this for app-parity setup; `sync_time` keeps the older
    /// `u64 unix + timezone` shape used by earlier probes.
    pub async fn sync_time_app(&self) -> Result<()> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let token = (now as u8).wrapping_mul(37).wrapping_add(0xa5);
        self.request(&protocol::req_sync_time_counter(now, token))
            .await?;
        Ok(())
    }

    /// Enable the async notification flags so the ring pushes events.
    pub async fn set_notification(&self, flags: u8) -> Result<()> {
        self.request(&protocol::req_set_notification(flags)).await?;
        Ok(())
    }

    /// Run the app-observed stream registration/capability-read sequence. Ring 5
    /// accepts these commands; they make the session closer to the official app
    /// before event fetches or live feature work.
    pub async fn setup_app_stream(&self) -> Result<()> {
        let hw = self.hardware_id().await.unwrap_or_default();
        let is_ring5 = hw.rsplit('_').next() == Some("05");
        if !is_ring5 {
            tracing::debug!("skipping Ring 5 app-stream setup for hardware_id={hw:?}");
            return Ok(());
        }
        self.request_tag(&protocol::req_stream_subscribe(0x02), 0x17)
            .await?;
        // Official app category masks observed by open_ring. These are
        // registrations, not feature-mode changes.
        for (category, flags) in [
            (0x14, 0x1000),
            (0x18, 0x1000),
            (0x28, 0x0900),
            (0x34, 0x0400),
            (0x04, 0x1000),
            (0x08, 0x1000),
        ] {
            self.request_tag(&protocol::req_event_subscribe(category, flags), 0x19)
                .await?;
        }
        // Parameter reads answer with ext 0x21; the `2f 02 03 01` state poll with
        // ext 0x04 (both confirmed in on-device captures).
        for (req, ext) in [
            (protocol::req_param_read(0x02), 0x21),
            (protocol::req_param_read(0x04), 0x21),
            (vec![0x2f, 0x02, 0x03, 0x01], 0x04),
            (protocol::req_param_read(0x0b), 0x21),
            (protocol::req_param_read(0x0d), 0x21),
            (protocol::req_param_read(0x03), 0x21),
            (protocol::req_param_read(0x0b), 0x21),
            (protocol::req_param_read(0x10), 0x21),
        ] {
            self.request_ext(&req, ext).await?;
        }
        Ok(())
    }

    // --- history events ----------------------------------------------------

    /// Drain history events starting from `cursor` (deciseconds), invoking
    /// `on_event` for each. Loops until the ring reports no bytes left. Returns
    /// the count synced and the next cursor to persist for incremental sync.
    ///
    /// `on_batch` is called after every fully-processed batch with the advanced
    /// cursor + the ring's remaining-bytes count, so callers can persist the
    /// cursor incrementally (an interrupted sync then resumes instead of
    /// re-pulling) and surface progress to the user.
    ///
    /// A batch that ends without the ring's summary packet is a hard error, not
    /// a completed sync: the summary is the ring's explicit terminator, so its
    /// absence means the link died mid-batch. Callers should reconnect and call
    /// again — the persisted cursor makes that resume, not restart.
    pub async fn drain_events<F, G>(
        &self,
        cursor: u32,
        mut on_event: F,
        mut on_batch: G,
    ) -> Result<SyncOutcome>
    where
        F: FnMut(&RingEvent) -> bool,
        G: FnMut(&BatchProgress) -> bool,
    {
        let mut start = cursor;
        let mut total = 0u32;
        // Prefer Ring 5's Android-style extended event drain. It falls back to
        // legacy GetEvent if the ring explicitly reports the extended API as
        // unsupported.
        let mut use_extended = true;
        // A batch is over when the ring's summary packet arrives (0x11 legacy /
        // ext 0x42) — the same terminator the official app waits for. An ext
        // status packet (payload[0]=0x00, "unsupported") also ends the request.
        let batch_terminal = |p: &Packet| {
            p.tag == 0x11 || (p.tag == 0x2f && matches!(p.payload.first(), Some(0x42) | Some(0x00)))
        };
        // Safety bound against a misbehaving ring that never reports drained.
        for _ in 0..100_000 {
            let mut packets = self.request_tag(&protocol::req_data_flush(), 0x29).await?;
            if use_extended {
                let ext = self
                    .request_batch(
                        &protocol::req_ext_get_event((start as u64) * 100, EXT_BATCH_MAX_EVENTS, 0),
                        batch_terminal,
                    )
                    .await?;
                let unsupported = ext
                    .iter()
                    .any(|p| p.tag == 0x2f && p.payload.first().copied() == Some(0x00));
                if unsupported {
                    use_extended = false;
                    packets.extend(
                        self.request_batch(
                            &protocol::req_get_event(start, 255, -1),
                            batch_terminal,
                        )
                        .await?,
                    );
                } else {
                    packets.extend(ext);
                }
            } else {
                packets.extend(
                    self.request_batch(&protocol::req_get_event(start, 255, -1), batch_terminal)
                        .await?,
                );
            }

            let mut max_ts = start;
            let mut batch_events = 0u32;
            let batch = decode_batch(&packets).map_err(|e| {
                Error::Protocol(format!(
                    "{e} — BLE link lost mid-batch? cursor {start} is checkpointed; \
                     reconnect and sync again to resume"
                ))
            })?;
            for ev in batch.events {
                if !on_event(&ev) {
                    return Err(Error::Protocol(
                        "event callback failed; not acknowledging batch".into(),
                    ));
                }
                max_ts = max_ts.max(ev.timestamp);
                batch_events += 1;
                total += 1;
            }
            let bytes_left = batch.bytes_left;
            // Advance the cursor past the newest event seen.
            let next = max_ts.saturating_add(1);
            let progressed = batch_events > 0 && next > start;
            if progressed {
                start = next;
            }
            // Report every batch (even an empty terminal one) so callers can
            // persist the cursor and show progress.
            if !on_batch(&BatchProgress {
                next_cursor: start,
                bytes_left,
                events_synced: total,
            }) {
                return Err(Error::Protocol(
                    "batch callback failed; not acknowledging batch".into(),
                ));
            }
            if progressed {
                let _ = self
                    .request_tag(&protocol::req_get_event_ack(start), 0x11)
                    .await;
            }
            if bytes_left == 0 {
                break; // drained
            }
            if !progressed {
                return Err(Error::Protocol(format!(
                    "ring reports {bytes_left} bytes of events left but the batch \
                     contained none — stopping instead of looping (cursor {start})"
                )));
            }
        }
        Ok(SyncOutcome {
            events_synced: total,
            next_cursor: start,
        })
    }

    // --- live / latest -----------------------------------------------------

    /// Read a feature's latest cached values (HR / SpO2). Reflects the last
    /// automatic measurement; meaningful only when the ring is worn.
    pub async fn feature_latest(&self, feature_id: u8) -> Result<LatestValues> {
        let packets = self
            .request_ext(&protocol::req_feature_latest(feature_id), 0x25)
            .await?;
        let p = packets
            .iter()
            .find(|p| p.ext_tag() == Some(0x25))
            .ok_or_else(|| Error::Protocol("no feature-latest response".into()))?;
        // payload: [0]=0x25,[1]=feature,[2]=result,[3]=status,[4]=state,
        //          [5..7]=counter, [7..]=feature-specific data.
        let data = p.payload.get(7..).unwrap_or(&[]);
        let mut out = LatestValues::default();
        match feature_id {
            feature::DAYTIME_HR => {
                // data[0..2] = rr-corrected IBI (ms); bpm = 60000 / ibi.
                if data.len() >= 2 {
                    let ibi = u16::from_le_bytes([data[0], data[1]]);
                    out.bpm = bpm_from_ibi(ibi);
                }
            }
            feature::EXERCISE_HR => {
                // data[4] = last HR value (bpm).
                if let Some(&bpm) = data.get(4) {
                    if bpm > 0 {
                        out.bpm = Some(bpm as u16);
                    }
                }
            }
            feature::SPO2 => {
                // data[3] = SpO2 %, data[4] = HR bpm.
                if let Some(&spo2) = data.get(3) {
                    if spo2 > 0 {
                        out.spo2_percent = Some(spo2);
                    }
                }
                if let Some(&bpm) = data.get(4) {
                    if bpm > 0 {
                        out.bpm = Some(bpm as u16);
                    }
                }
            }
            _ => {}
        }
        Ok(out)
    }

    /// Trigger the ring's sleep analysis. Returns the `0x29` status byte.
    pub async fn check_sleep_analysis(&self, force: bool) -> Result<u8> {
        let packets = self
            .request(&protocol::req_check_sleep_analysis(force))
            .await?;
        Self::find(&packets, 0x29)
            .and_then(|p| p.payload.first().copied())
            .ok_or_else(|| Error::Protocol("no sleep-analysis response".into()))
    }

    /// Read a feature's status (mode/state/subscription).
    pub async fn feature_status(&self, feature_id: u8) -> Result<FeatureStatus> {
        let packets = self
            .request(&protocol::req_feature_status(feature_id))
            .await?;
        packets
            .iter()
            .find_map(FeatureStatus::parse)
            .ok_or_else(|| Error::Protocol("no feature-status response".into()))
    }

    /// Set a feature's mode (e.g. `feature_mode::AUTOMATIC` to enable measurement).
    pub async fn set_feature_mode(&self, feature_id: u8, mode: u8) -> Result<()> {
        let packets = self
            .request(&protocol::req_set_feature_mode(feature_id, mode))
            .await?;
        match packets
            .iter()
            .find(|p| p.ext_tag() == Some(0x23))
            .and_then(|p| p.payload.get(2).copied())
        {
            Some(0x00) => Ok(()),
            Some(other) => Err(Error::Protocol(format!(
                "set_feature_mode result {other:#04x}"
            ))),
            None => Err(Error::Protocol("no set_feature_mode response".into())),
        }
    }

    /// Subscribe/unsubscribe a feature capability (e.g. real steps, Atlas/bioZ) via
    /// `SetFeatureSubscription`. Returns the ring's raw result byte (0 = success;
    /// non-zero = rejected, e.g. feature disabled in firmware) so the caller can see
    /// exactly how the ring responds.
    pub async fn set_feature_subscription(&self, capability: u8, mode: u8) -> Result<u8> {
        let packets = self
            .request(&protocol::req_set_feature_subscription(capability, mode))
            .await?;
        packets
            .iter()
            .find(|p| p.ext_tag() == Some(0x27))
            .and_then(|p| p.payload.get(2).copied())
            .ok_or_else(|| Error::Protocol("no set_feature_subscription response".into()))
    }

    /// Query the RData collection state (read-only). Returns `(subtag, status)`.
    pub async fn rdata_state(&self) -> Result<(u8, u8)> {
        let packets = self.request(&protocol::req_rdata_state()).await?;
        Self::find(&packets, 0x03)
            .and_then(|p| Some((*p.payload.first()?, *p.payload.get(1)?)))
            .ok_or_else(|| Error::Protocol("no RData state response (auth required?)".into()))
    }

    /// Stop an active RData collection session (part of mandatory teardown).
    /// Returns the response status byte (255 if absent).
    pub async fn rdata_stop(&self) -> Result<u8> {
        let packets = self.request(&protocol::req_rdata_stop()).await?;
        Ok(Self::find(&packets, 0x03)
            .and_then(|p| p.payload.get(1).copied())
            .unwrap_or(255))
    }

    /// Clear the RData session/data from the ring's flash (part of teardown).
    /// Returns the response status byte (255 if absent).
    pub async fn rdata_clear(&self) -> Result<u8> {
        let packets = self.request(&protocol::req_rdata_clear()).await?;
        Ok(Self::find(&packets, 0x03)
            .and_then(|p| p.payload.get(1).copied())
            .unwrap_or(255))
    }

    /// Configure/arm an RData session for one or more signal types. **This starts
    /// persistent flash sampling that does NOT self-stop** — the caller is
    /// responsible for the `stop`+`clear` teardown. Returns `(subtag, status)`.
    pub async fn rdata_configure(
        &self,
        types: &[protocol::rdata::DataType],
        start_unix: u32,
        current_unix: u32,
    ) -> Result<(u8, u8)> {
        let packets = self
            .request(&protocol::req_rdata_configure(
                types,
                start_unix,
                current_unix,
            ))
            .await?;
        Self::find(&packets, 0x03)
            .and_then(|p| Some((*p.payload.first()?, *p.payload.get(1)?)))
            .ok_or_else(|| Error::Protocol("no RData configure response".into()))
    }

    /// Fetch one RData page by index. Returns `(status, page_bytes)` where
    /// `status` is the subtag-status byte (`6` = NO_DATA / past the end) and
    /// `page_bytes` is the payload after the `[subtag, status]` header.
    pub async fn rdata_get_page(&self, page: u16) -> Result<(u8, Vec<u8>)> {
        let packets = self.request(&protocol::req_rdata_get_page(page)).await?;
        Self::find(&packets, 0x03)
            .map(|p| {
                let status = p.payload.get(1).copied().unwrap_or(0);
                let bytes = p.payload.get(2..).unwrap_or(&[]).to_vec();
                (status, bytes)
            })
            .ok_or_else(|| Error::Protocol("no RData page response".into()))
    }

    /// Enable live heart rate (daytime HR, `CONNECTED_LIVE`) and invoke `on_sample`
    /// for each valid beat for up to `duration`. Restores `AUTOMATIC` mode on exit.
    /// The ring must be worn for samples to appear.
    pub async fn live_heart_rate<F>(
        &self,
        duration: Duration,
        debug: bool,
        mut on_sample: F,
    ) -> Result<()>
    where
        F: FnMut(HeartRateSample),
    {
        let mut rx = self.transport.subscribe();
        // Drain backlog.
        while rx.try_recv().is_ok() {}

        self.transport
            .write(&protocol::req_set_feature_mode(
                feature::DAYTIME_HR,
                feature_mode::CONNECTED_LIVE,
            ))
            .await?;

        let deadline = tokio::time::Instant::now() + duration;
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                break;
            }
            match tokio::time::timeout(remaining, rx.recv()).await {
                Ok(Ok(frame)) => {
                    if debug {
                        eprintln!("raw notify: {}", hex::encode(&frame));
                    }
                    if let Some(sample) = parse_live_hr_frame(&frame) {
                        on_sample(sample);
                    }
                }
                Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => continue,
                _ => break,
            }
        }

        // Best-effort restore to automatic mode.
        let _ = self
            .transport
            .write(&protocol::req_set_feature_mode(
                feature::DAYTIME_HR,
                feature_mode::AUTOMATIC,
            ))
            .await;
        Ok(())
    }

    /// Stream live accelerometer samples (the "wave to test motion" path): enable
    /// the ACM real-time measurement for `duration` and invoke `on_sample` for each
    /// x/y/z reading. The request is time-boxed (minutes) so the ring auto-stops,
    /// and we also send an explicit OFF on exit. The ring must be worn/moving.
    pub async fn stream_accelerometer<F>(&self, duration: Duration, mut on_sample: F) -> Result<()>
    where
        F: FnMut(AcmSample),
    {
        let mut rx = self.transport.subscribe();
        while rx.try_recv().is_ok() {}

        let minutes = (duration.as_secs().div_ceil(60)).max(1) as u16;
        self.transport
            .write(&protocol::req_set_realtime(
                protocol::realtime::ACM,
                minutes,
                0,
            ))
            .await?;

        let deadline = tokio::time::Instant::now() + duration;
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                break;
            }
            match tokio::time::timeout(remaining, rx.recv()).await {
                Ok(Ok(frame)) => {
                    for sample in parse_acm_frame(&frame) {
                        on_sample(sample);
                    }
                }
                Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => continue,
                _ => break,
            }
        }

        // Mandatory teardown: real-time measurements do not self-stop reliably.
        let _ = self.transport.write(&protocol::req_realtime_off()).await;
        Ok(())
    }
}

/// Parse an ACM measurement indication (response tag `0x33`) into up to 2 samples.
///
/// Frame: `[0]=0x33 [1]=len [2]=sampleRate [3]=seq [4..10]=x,y,z [10..16]=x,y,z?`,
/// each axis a signed `i16` little-endian.
fn parse_acm_frame(frame: &[u8]) -> Vec<AcmSample> {
    let mut out = Vec::new();
    if frame.len() < 10 || frame[0] != protocol::realtime::ACM_RESPONSE_TAG {
        return out;
    }
    let s = |o: usize| i16::from_le_bytes([frame[o], frame[o + 1]]);
    out.push(AcmSample {
        x: s(4),
        y: s(6),
        z: s(8),
    });
    if frame.len() >= 16 {
        out.push(AcmSample {
            x: s(10),
            y: s(12),
            z: s(14),
        });
    }
    out
}

/// Compute bpm from an inter-beat interval, ignoring implausible values.
fn bpm_from_ibi(ibi_ms: u16) -> Option<u16> {
    if (300..=2000).contains(&ibi_ms) {
        Some((60_000u32 / ibi_ms as u32) as u16)
    } else {
        None
    }
}

/// Parse a daytime-HR live subscription notification (tag `0x2f`, sub-tag `0x28`).
///
/// Frame layout: `[0]=0x2f [1]=len [2]=0x28(IND1) [3]=cap [4]=status [5]=state
/// [6..8]=timeSince [8..10]=IBI`. The IBI word packs a 12-bit interval (ms) and a
/// 4-bit validity nibble (1 = VALID), per the app's `IBI` decoder.
fn parse_live_hr_frame(frame: &[u8]) -> Option<HeartRateSample> {
    if frame.len() < 10 || frame[0] != 0x2f || frame[2] != 0x28 {
        return None;
    }
    if frame[3] != feature::DAYTIME_HR {
        return None;
    }
    let lo = frame[8];
    let hi = frame[9];
    let ibi_ms = (((hi & 0x0f) as u16) << 8) | lo as u16;
    let validity = (hi >> 4) & 0x0f;
    if validity != 1 {
        return None;
    }
    bpm_from_ibi(ibi_ms).map(|bpm| HeartRateSample { bpm, ibi_ms })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::mock::MockTransport;

    #[tokio::test]
    async fn reads_firmware_over_mock() {
        let mock = MockTransport::new();
        mock.on("0803000000", &["091202000003040301000105000cffeeddccbbaa"]);
        let client = OuraClient::new(mock).with_quiet(Duration::from_millis(20));
        let info = client.firmware().await.unwrap();
        assert_eq!(info.firmware_version, "3.4.3");
    }

    #[tokio::test]
    async fn authenticates_over_mock() {
        let mock = MockTransport::new();
        mock.on("2f012b", &["2f102c0e2d6a0a08c99b4365f458e6e97382"]);
        // The encrypted authenticate request for this key+nonce, then success.
        mock.on("2f112da38a8772d3acb6db5c2b516dd56987c8", &["2f022e00"]);
        let client = OuraClient::new(mock).with_quiet(Duration::from_millis(20));
        let key: [u8; 16] = hex::decode("4431967d8bacc2659743142b68391d9a")
            .unwrap()
            .try_into()
            .unwrap();
        assert_eq!(
            client.authenticate(&key).await.unwrap(),
            AuthResult::Success
        );
    }

    #[test]
    fn acm_frame_decodes_two_samples() {
        // 33 0c 32 01 | 0100 0200 0300 | 0400 0500 0600
        let frame = [0x33, 0x0c, 0x32, 0x01, 1, 0, 2, 0, 3, 0, 4, 0, 5, 0, 6, 0];
        let s = parse_acm_frame(&frame);
        assert_eq!(s.len(), 2);
        assert_eq!((s[0].x, s[0].y, s[0].z), (1, 2, 3));
        assert_eq!((s[1].x, s[1].y, s[1].z), (4, 5, 6));
    }

    #[test]
    fn live_hr_frame_decodes() {
        // ibi=857ms (0x359), validity=1 -> hi=0x13, lo=0x59; bpm=60000/857=70
        let frame = [0x2f, 0x08, 0x28, 0x02, 0x00, 0x02, 0x00, 0x00, 0x59, 0x13];
        let s = parse_live_hr_frame(&frame).unwrap();
        assert_eq!(s.ibi_ms, 857);
        assert_eq!(s.bpm, 70);
    }
}
