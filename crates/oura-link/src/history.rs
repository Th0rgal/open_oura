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
}
