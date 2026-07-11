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
}

impl ValidatedHistoryBatch {
    pub fn progressed(&self, previous_cursor: u32) -> bool {
        !self.events.is_empty() && self.next_cursor > previous_cursor
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
    let events = batch
        .events
        .into_iter()
        .filter(|event| {
            if plausible_timestamp(batch_start, event.timestamp) {
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
    }
}

pub(crate) fn decode_batch(packets: &[Packet]) -> Result<HistoryBatch> {
    let mut bytes_left = None;
    let mut events = Vec::new();
    let mut envelopes = ExtEventEnvelopeParser::default();

    for packet in packets {
        if packet.tag == 0x11 {
            bytes_left = EventBatchSummary::parse(packet).map(|s| s.bytes_left);
        } else if packet.tag == 0x2f && packet.payload.first() == Some(&0x42) {
            bytes_left = ExtEventBatchSummary::parse(packet).map(|s| s.bytes_left);
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
