//! [`OuraClient`] — the high-level, transport-generic API.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use oura_protocol::auth::{encrypt_nonce, AuthResult};
use oura_protocol::device::{self, Battery, Capability, DeviceInfo};
use crate::error::{Error, Result};
use oura_protocol::events::{EventBatchSummary, RingEvent};
use oura_protocol::protocol::{self, feature, feature_mode, Packet};
use crate::transport::{transact, Transport};

/// Default quiet window for collecting responses to a request.
pub const DEFAULT_QUIET: Duration = Duration::from_millis(1500);

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
        let frames = transact(&self.transport, bytes, self.quiet).await?;
        Ok(frames.iter().filter_map(|f| Packet::parse(f)).collect())
    }

    fn find(packets: &[Packet], tag: u8) -> Option<&Packet> {
        packets.iter().find(|p| p.tag == tag)
    }

    // --- device info -------------------------------------------------------

    /// Read firmware/version metadata (no auth required).
    pub async fn firmware(&self) -> Result<DeviceInfo> {
        let packets = self.request(&protocol::req_firmware()).await?;
        Self::find(&packets, 0x09)
            .and_then(DeviceInfo::parse)
            .ok_or_else(|| Error::Protocol("no firmware response".into()))
    }

    /// Read battery state (requires app-auth on rings with a key installed).
    pub async fn battery(&self) -> Result<Battery> {
        let packets = self.request(&protocol::req_battery()).await?;
        Self::find(&packets, 0x0d)
            .and_then(Battery::parse)
            .ok_or_else(|| Error::Protocol("no battery response (auth required?)".into()))
    }

    /// Read the ring serial number.
    pub async fn serial(&self) -> Result<String> {
        let packets = self.request(&protocol::product::SERIAL).await?;
        Self::find(&packets, 0x19)
            .and_then(device::parse_product_ascii)
            .ok_or_else(|| Error::Protocol("no serial response".into()))
    }

    /// Read the hardware id (e.g. `BLB_03`).
    pub async fn hardware_id(&self) -> Result<String> {
        let packets = self.request(&protocol::product::HARDWARE).await?;
        Self::find(&packets, 0x19)
            .and_then(device::parse_product_ascii)
            .ok_or_else(|| Error::Protocol("no hardware response".into()))
    }

    /// Read both capability pages.
    pub async fn capabilities(&self) -> Result<Vec<Capability>> {
        let mut caps = Vec::new();
        for page in 0u8..2 {
            let packets = self.request(&protocol::req_capabilities(page)).await?;
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
        let packets = self.request(&protocol::req_auth_nonce()).await?;
        let nonce = packets
            .iter()
            .find(|p| p.ext_tag() == Some(0x2c))
            .map(|p| p.payload[1..].to_vec())
            .ok_or_else(|| Error::Auth("no nonce response".into()))?;

        let encrypted = encrypt_nonce(key, &nonce);
        let packets = self.request(&protocol::req_authenticate(&encrypted)).await?;
        let state = packets
            .iter()
            .find(|p| p.ext_tag() == Some(0x2e))
            .and_then(|p| p.payload.get(1).copied())
            .ok_or_else(|| Error::Auth("no authenticate response".into()))?;

        let result = AuthResult::from(state);
        if result.is_success() {
            Ok(result)
        } else {
            Err(Error::Auth(format!("{result:?}")))
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

    /// Enable the async notification flags so the ring pushes events.
    pub async fn set_notification(&self, flags: u8) -> Result<()> {
        self.request(&protocol::req_set_notification(flags)).await?;
        Ok(())
    }

    // --- history events ----------------------------------------------------

    /// Drain history events starting from `cursor` (deciseconds), invoking
    /// `on_event` for each. Loops until the ring reports no bytes left. Returns
    /// the count synced and the next cursor to persist for incremental sync.
    pub async fn drain_events<F>(&self, cursor: u32, mut on_event: F) -> Result<SyncOutcome>
    where
        F: FnMut(&RingEvent),
    {
        let mut start = cursor;
        let mut total = 0u32;
        // Safety bound against a misbehaving ring that never reports drained.
        for _ in 0..100_000 {
            let packets = self
                .request(&protocol::req_get_event(start, 255, -1))
                .await?;

            let mut summary: Option<EventBatchSummary> = None;
            let mut max_ts = start;
            let mut batch_events = 0u32;
            for p in &packets {
                if p.tag == 0x11 {
                    summary = EventBatchSummary::parse(p);
                } else if p.tag >= protocol::HISTORY_EVENT_PREFIX {
                    let ev = RingEvent::from_packet(p);
                    max_ts = max_ts.max(ev.timestamp);
                    batch_events += 1;
                    total += 1;
                    on_event(&ev);
                }
            }

            let bytes_left = summary.map(|s| s.bytes_left).unwrap_or(0);
            // Advance the cursor past the newest event seen.
            let next = max_ts.saturating_add(1);
            let progressed = batch_events > 0 && next > start;
            if progressed {
                start = next;
            }
            // Stop when drained, or when we can make no further progress.
            if bytes_left == 0 || !progressed {
                break;
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
        let packets = self.request(&protocol::req_feature_latest(feature_id)).await?;
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
        let packets = self.request(&protocol::req_feature_status(feature_id)).await?;
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
            Some(other) => Err(Error::Protocol(format!("set_feature_mode result {other:#04x}"))),
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
    pub async fn rdata_stop(&self) -> Result<()> {
        self.request(&protocol::req_rdata_stop()).await?;
        Ok(())
    }

    /// Clear the RData session/data from the ring's flash (part of teardown).
    pub async fn rdata_clear(&self) -> Result<()> {
        self.request(&protocol::req_rdata_clear()).await?;
        Ok(())
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
        mock.on(
            "0803000000",
            &["091202000003040301000105000ca56c2af838a0"],
        );
        let client = OuraClient::new(mock).with_quiet(Duration::from_millis(20));
        let info = client.firmware().await.unwrap();
        assert_eq!(info.firmware_version, "3.4.3");
    }

    #[tokio::test]
    async fn authenticates_over_mock() {
        let mock = MockTransport::new();
        mock.on("2f012b", &["2f102c0e2d6a0a08c99b4365f458e6e97382"]);
        // The encrypted authenticate request for this key+nonce, then success.
        mock.on(
            "2f112da38a8772d3acb6db5c2b516dd56987c8",
            &["2f022e00"],
        );
        let client = OuraClient::new(mock).with_quiet(Duration::from_millis(20));
        let key: [u8; 16] = hex::decode("4431967d8bacc2659743142b68391d9a")
            .unwrap()
            .try_into()
            .unwrap();
        assert_eq!(client.authenticate(&key).await.unwrap(), AuthResult::Success);
    }

    #[test]
    fn acm_frame_decodes_two_samples() {
        // 33 0c 32 01 | 0100 0200 0300 | 0400 0500 0600
        let frame = [
            0x33, 0x0c, 0x32, 0x01, 1, 0, 2, 0, 3, 0, 4, 0, 5, 0, 6, 0,
        ];
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
