//! Pure history-batch decoding, kept separate from transport/checkpoint orchestration.

use crate::error::{Error, Result};
use oura_protocol::events::{
    EventBatchSummary, ExtEventBatchSummary, ExtEventEnvelopeParser, RingEvent,
};
use oura_protocol::protocol::{self, Packet};

#[derive(Debug)]
pub(crate) struct HistoryBatch {
    pub events: Vec<RingEvent>,
    pub bytes_left: u32,
}

/// Result of validating a decoded batch against the checkpoint that requested it.
///
/// Keeping this decision in the pure history layer prevents malformed transport
/// envelopes from leaking into storage or cursor orchestration.
#[derive(Debug)]
pub(crate) struct ValidatedHistoryBatch {
    pub events: Vec<RingEvent>,
    pub bytes_left: u32,
    pub next_cursor: u32,
    pub rejected_events: u32,
    /// Events at or after the cursor the batch was requested with — the only
    /// ones that represent forward progress through the ring's history.
    pub fresh_events: u32,
}

impl ValidatedHistoryBatch {
    /// Whether the drain may continue from [`Self::next_cursor`].
    ///
    /// A batch containing only events OLDER than the cursor is the ring ignoring
    /// the cursor and replaying from its buffer head. That case still leaves
    /// `next_cursor` one decisecond past `previous_cursor`, because the cursor is
    /// seeded from the batch start, so treating "cursor moved" as progress made
    /// the drain crawl forward 0.1s per round trip while re-serving the same
    /// history — unbounded event counts, hours of transfer, and a database that
    /// deduplicates all of it away. Real progress requires a fresh event.
    pub fn progressed(&self, previous_cursor: u32) -> bool {
        self.fresh_events > 0 && self.next_cursor > previous_cursor
    }
}

// A retained history batch may legitimately jump over quiet periods, but never by
// years. Corrupt/misaligned extended envelopes otherwise poison the persisted cursor
// with a near-u32::MAX timestamp and force every later sync to replay from zero.
const MAX_CURSOR_ADVANCE_DS: u32 = 180 * 24 * 60 * 60 * 10;

fn plausible_timestamp(batch_start: u32, timestamp: u32) -> bool {
    timestamp <= batch_start.saturating_add(MAX_CURSOR_ADVANCE_DS)
}

pub(crate) fn validate_batch(batch: HistoryBatch, batch_start: u32) -> ValidatedHistoryBatch {
    let bytes_left = batch.bytes_left;
    let mut max_timestamp = batch_start;
    let mut rejected_events = 0;
    let mut fresh_events = 0;
    let events = batch
        .events
        .into_iter()
        .filter(|event| {
            if plausible_timestamp(batch_start, event.timestamp) {
                if event.timestamp >= batch_start {
                    fresh_events += 1;
                }
                max_timestamp = max_timestamp.max(event.timestamp);
                true
            } else {
                rejected_events += 1;
                false
            }
        })
        .collect::<Vec<_>>();

    ValidatedHistoryBatch {
        events,
        bytes_left,
        next_cursor: max_timestamp.saturating_add(1),
        rejected_events,
        fresh_events,
    }
}

pub(crate) fn decode_batch(packets: &[Packet]) -> Result<HistoryBatch> {
    let mut bytes_left = None;
    let mut result_code = None;
    let mut events = Vec::new();
    let mut envelopes = ExtEventEnvelopeParser::default();

    for packet in packets {
        if packet.tag == 0x11 {
            bytes_left = EventBatchSummary::parse(packet).map(|s| s.bytes_left);
        } else if packet.tag == 0x2f && packet.payload.first() == Some(&0x42) {
            if let Some(summary) = ExtEventBatchSummary::parse(packet) {
                bytes_left = Some(summary.bytes_left);
                result_code = Some(summary.result_code);
            }
        } else if packet.tag == 0x2f && packet.payload.first() == Some(&0x43) {
            events.extend(
                envelopes
                    .push_packet(packet)
                    .into_iter()
                    .filter(|p| p.tag >= protocol::HISTORY_EVENT_PREFIX)
                    .map(|p| RingEvent::from_packet(&p)),
            );
        } else if packet.tag >= protocol::HISTORY_EVENT_PREFIX {
            events.push(RingEvent::from_packet(packet));
        }
    }

    let Some(bytes_left) = bytes_left else {
        return Err(Error::Protocol(format!(
            "event batch ended without a summary packet ({} packet(s) received)",
            packets.len()
        )));
    };
    if let Some(code) = result_code.filter(|&code| code != 0) {
        return Err(Error::Protocol(format!(
            "extended history request failed with result code 0x{code:02x}"
        )));
    }
    Ok(HistoryBatch { events, bytes_left })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn packet(hex_string: &str) -> Packet {
        Packet::parse(&hex::decode(hex_string).unwrap()).unwrap()
    }

    #[test]
    fn decodes_legacy_event_and_summary() {
        let batch =
            decode_batch(&[packet("43086400000074657374"), packet("1106000000000000")]).unwrap();
        assert_eq!(batch.events.len(), 1);
        assert_eq!(batch.events[0].timestamp, 100);
        assert_eq!(batch.bytes_left, 0);
    }

    #[test]
    fn decodes_extended_envelope_and_summary() {
        let batch = decode_batch(&[
            packet("2f09430600aa430364bbcc"),
            packet("2f0a42010000000000000000"),
        ])
        .unwrap();
        assert_eq!(batch.events.len(), 1);
        assert_eq!(batch.events[0].timestamp, 1);
        assert_eq!(batch.events[0].body, [0xbb, 0xcc]);
        assert_eq!(batch.bytes_left, 0);
    }

    #[test]
    fn rejects_unterminated_batch() {
        let error = decode_batch(&[packet("43086400000074657374")]).unwrap_err();
        assert!(error.to_string().contains("without a summary"));
    }

    #[test]
    fn rejects_extended_result_code_from_ios_sync_vector() {
        // Exact terminal frame from the 2026-07-11 iOS sync. 0xff is a rejected
        // request, not a successful empty batch, even though bytes_left is zero.
        let error = decode_batch(&[packet("2f0a420000000000000000ff")]).unwrap_err();
        assert!(error.to_string().contains("result code 0xff"));
    }

    #[test]
    fn a_batch_of_only_older_events_is_not_progress() {
        // The 2026-09-21 failure: every event in the batch predates the cursor, so
        // max_timestamp stays at batch_start and next_cursor lands exactly one
        // decisecond ahead. That looked like progress and made the drain crawl,
        // re-serving the same history until the event count ran past 300k.
        let batch_start = 1_000_000;
        let batch = HistoryBatch {
            events: vec![
                RingEvent { tag: 0x7e, name: "x".into(), timestamp: 900_000, body: vec![1], decoded: None },
                RingEvent { tag: 0x7e, name: "x".into(), timestamp: 950_000, body: vec![2], decoded: None },
            ],
            bytes_left: 4096,
        };
        let validated = validate_batch(batch, batch_start);

        assert_eq!(validated.fresh_events, 0);
        assert_eq!(validated.next_cursor, batch_start + 1, "cursor still nudges forward");
        assert!(
            !validated.progressed(batch_start),
            "a nudged cursor with no fresh event must not count as progress"
        );
    }

    #[test]
    fn an_event_at_the_cursor_is_progress() {
        let batch_start = 1_000_000;
        let batch = HistoryBatch {
            events: vec![
                RingEvent { tag: 0x7e, name: "x".into(), timestamp: 900_000, body: vec![1], decoded: None },
                RingEvent { tag: 0x7e, name: "x".into(), timestamp: 1_000_000, body: vec![2], decoded: None },
            ],
            bytes_left: 4096,
        };
        let validated = validate_batch(batch, batch_start);

        assert_eq!(validated.fresh_events, 1);
        assert!(validated.progressed(batch_start));
    }

    #[test]
    fn rejects_cursor_poison_from_physical_ring_5_vector() {
        // Exact tail records extracted from a physical Ring 5 drain on 2026-07-11.
        // The first record is the last well-formed event; the remaining six came
        // from misaligned extended envelopes and contained fragments of later events.
        let batch_start = 6_906_561;
        let records = [
            (0x7e, 6_906_620),
            (0xd1, 2_390_248_269),
            (0x8c, 3_970_646_416),
            (0x46, 3_970_646_462),
            (0x60, 3_970_646_516),
            (0x45, 3_970_646_537),
            (0x61, 3_970_646_538),
        ];
        let batch = HistoryBatch {
            events: records
                .into_iter()
                .map(|(tag, timestamp)| RingEvent {
                    tag,
                    name: oura_protocol::events::event_name(tag),
                    timestamp,
                    body: Vec::new(),
                    decoded: None,
                })
                .collect(),
            bytes_left: 0,
        };

        let validated = validate_batch(batch, batch_start);

        assert_eq!(validated.events.len(), 1);
        assert_eq!(validated.events[0].tag, 0x7e);
        assert_eq!(validated.next_cursor, 6_906_621);
        assert_eq!(validated.rejected_events, 6);
        assert!(validated.progressed(batch_start));
    }
}
