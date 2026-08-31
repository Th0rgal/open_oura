//! `btleplug`-backed [`Transport`] for real rings (feature `ble`).

use std::collections::HashMap;
use std::time::{Duration, Instant};

use btleplug::api::{
    Central, CharPropFlags, Characteristic, Manager as _, Peripheral as _, PeripheralProperties,
    ScanFilter, WriteType,
};
use btleplug::platform::{Manager, Peripheral};
use futures::StreamExt;
use tokio::sync::broadcast;
use uuid::Uuid;

use crate::error::{Error, Result};
use crate::transport::Transport;
use oura_protocol::protocol;

/// Upper bound on the BLE connect + service-discovery + subscribe phase. CoreBluetooth
/// imposes no deadline itself, so without this a ring that won't complete the GATT
/// handshake hangs the caller forever.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

/// Bluetooth SIG company identifier for Oura Health Oy (NRF: `<0x02B2>`).
const OURA_COMPANY_ID: u16 = 0x02B2;

/// A ring discovered while scanning.
#[derive(Clone, Debug)]
pub struct Discovered {
    pub id: String,
    pub name: String,
    pub rssi: i16,
}

/// A connected BLE link to a ring. Notifications from every notify/indicate
/// characteristic in the Oura service are merged into one broadcast stream, which
/// keeps the client working across ring generations that expose extra
/// characteristics (Ring 5 adds `…0004/0005/0006`).
pub struct BleTransport {
    peripheral: Peripheral,
    write_char: Characteristic,
    tx: broadcast::Sender<Vec<u8>>,
    _pump: tokio::task::JoinHandle<()>,
}

async fn first_adapter() -> Result<btleplug::platform::Adapter> {
    let manager = Manager::new().await?;
    manager
        .adapters()
        .await?
        .into_iter()
        .next()
        .ok_or_else(|| Error::Ble("no Bluetooth adapter found".into()))
}

/// True if advertisement has Oura-specific evidence (not merely a name substring).
///
/// Accepts either:
/// - Oura GATT service UUID in the adv service list, or
/// - Oura company id `0x02B2` in manufacturer data
///
/// Name alone is intentionally insufficient: on Windows, `connect` may trigger
/// an auto-accepted OS BLE bond before GATT validation, so a coincidental
/// `"oura"` substring in an unrelated device name must not select a candidate.
/// WinRT often omits incomplete 128-bit service lists from `props.services`
/// even when NRF shows them; manufacturer data is the reliable fallback there.
fn is_oura_advertisement(
    services: &[Uuid],
    manufacturer_data: &HashMap<u16, Vec<u8>>,
    _local_name: &str,
) -> bool {
    services.contains(&protocol::OURA_SERVICE)
        || manufacturer_data.contains_key(&OURA_COMPANY_ID)
}

/// Apply the user `--name` substring filter.
///
/// Empty needle matches everything. On Windows the local name is often missing
/// even when the ring advertises one; if the user asked for the default `"Oura"`
/// (or another `oura…` needle), allow empty-named candidates that already passed
/// [`is_oura_advertisement`].
fn name_filter_matches(name_contains: &str, local_name: &str) -> bool {
    let needle = name_contains.trim().to_lowercase();
    if needle.is_empty() {
        return true;
    }
    let name = local_name.to_lowercase();
    if name.contains(&needle) {
        return true;
    }
    name.is_empty() && needle.starts_with("oura")
}

fn props_match_oura(props: &PeripheralProperties, name_contains: &str) -> bool {
    let name = props.local_name.as_deref().unwrap_or("");
    is_oura_advertisement(&props.services, &props.manufacturer_data, name)
        && name_filter_matches(name_contains, name)
}

/// Scan for Oura rings. Uses an unfiltered OS scan then classifies advertisements
/// in-process so platforms that drop incomplete service UUID lists (notably
/// WinRT) still surface rings. Returns candidates sorted by RSSI (strongest first).
pub async fn scan(name_contains: &str, timeout: Duration) -> Result<Vec<Discovered>> {
    let adapter = first_adapter().await?;
    // Do not pass `services: [OURA_SERVICE]` to the OS watcher: on Windows that
    // filter can hide ads that only carry an *incomplete* 128-bit UUID list.
    adapter.start_scan(ScanFilter::default()).await?;

    let deadline = Instant::now() + timeout;
    let mut found: Vec<Discovered> = Vec::new();
    while Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(400)).await;
        for p in adapter.peripherals().await? {
            let Some(props) = p.properties().await? else {
                continue;
            };
            if !props_match_oura(&props, name_contains) {
                continue;
            }
            let name = props.local_name.unwrap_or_default();
            let id = p.id().to_string();
            let entry = Discovered {
                id: id.clone(),
                name,
                rssi: props.rssi.unwrap_or(i16::MIN),
            };
            match found.iter_mut().find(|d| d.id == id) {
                Some(existing) => *existing = entry,
                None => found.push(entry),
            }
        }
    }
    let _ = adapter.stop_scan().await;
    found.sort_by_key(|d| std::cmp::Reverse(d.rssi));
    Ok(found)
}

impl BleTransport {
    /// Scan for and connect to a ring, selecting the strongest match for
    /// `name_contains`. If `address` is given, only a device whose id matches is
    /// considered.
    pub async fn connect(
        name_contains: &str,
        address: Option<&str>,
        scan_timeout: Duration,
    ) -> Result<Self> {
        let adapter = first_adapter().await?;
        adapter.start_scan(ScanFilter::default()).await?;

        let deadline = Instant::now() + scan_timeout;
        let mut chosen: Option<(Peripheral, i16)> = None;
        while Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(400)).await;
            for p in adapter.peripherals().await? {
                let Some(props) = p.properties().await? else {
                    continue;
                };
                if !props_match_oura(&props, name_contains) {
                    continue;
                }
                if let Some(addr) = address {
                    if !p.id().to_string().eq_ignore_ascii_case(addr) {
                        continue;
                    }
                }
                let rssi = props.rssi.unwrap_or(i16::MIN);
                if chosen.as_ref().map(|(_, r)| rssi > *r).unwrap_or(true) {
                    chosen = Some((p, rssi));
                }
            }
            if chosen.is_some() {
                // brief settle to prefer the strongest advertiser
                tokio::time::sleep(Duration::from_millis(300)).await;
                break;
            }
        }
        let _ = adapter.stop_scan().await;

        let (peripheral, _) = chosen.ok_or(Error::DeviceNotFound)?;

        // Windows: OS BLE bond/link-encryption is required before encrypted GATT
        // characteristics can be used. Settings UI pairing often fails; request
        // the bond via WinRT before btleplug opens the ATT session. Bound so a
        // stalled OS ceremony cannot hang connect forever (prompts need headroom
        // beyond the GATT CONNECT_TIMEOUT).
        #[cfg(windows)]
        {
            // Must exceed windows_bond's worst case: optional 15s stale unpair +
            // 2×45s PairAsync attempts + up to 2×15s weak-bond unpairs + device open.
            const BOND_TIMEOUT: Duration = Duration::from_secs(180);
            let id = peripheral.id().to_string();
            tokio::time::timeout(BOND_TIMEOUT, crate::windows_bond::ensure_ble_bond(&id))
                .await
                .map_err(|_| Error::ConnectTimeout)??;
        }

        // CoreBluetooth's connect (and, on some stacks, service discovery) has no
        // deadline of its own: a ring that advertises but won't complete the GATT
        // handshake — e.g. because a phone still holds the single peripheral link —
        // makes these calls hang forever. Bound the whole setup phase so we fail with
        // an actionable error instead of blocking indefinitely.
        let write_char = tokio::time::timeout(CONNECT_TIMEOUT, async {
            if !peripheral.is_connected().await? {
                peripheral.connect().await?;
            }
            peripheral.discover_services().await?;

            let chars = peripheral.characteristics();
            let write_char = chars
                .iter()
                .find(|c| c.uuid == protocol::OURA_WRITE)
                .cloned()
                .ok_or_else(|| Error::CharacteristicNotFound(protocol::OURA_WRITE.to_string()))?;

            let notify_chars = chars.iter().filter(|c| {
                c.service_uuid == protocol::OURA_SERVICE
                    && c.properties
                        .intersects(CharPropFlags::NOTIFY | CharPropFlags::INDICATE)
            });
            for c in notify_chars {
                peripheral.subscribe(c).await?;
            }
            Ok::<_, Error>(write_char)
        })
        .await
        .map_err(|_| Error::ConnectTimeout)??;

        let (tx, _) = broadcast::channel(256);
        let pump_tx = tx.clone();
        let pump_peripheral = peripheral.clone();
        let pump = tokio::spawn(async move {
            if let Ok(mut stream) = pump_peripheral.notifications().await {
                while let Some(n) = stream.next().await {
                    // Best-effort fan-out; ignore if there are no live receivers.
                    let _ = pump_tx.send(n.value);
                }
            }
        });

        Ok(Self {
            peripheral,
            write_char,
            tx,
            _pump: pump,
        })
    }

    /// Disconnect from the ring.
    pub async fn disconnect(&self) -> Result<()> {
        self.peripheral.disconnect().await?;
        Ok(())
    }
}

#[async_trait::async_trait]
impl Transport for BleTransport {
    async fn write(&self, data: &[u8]) -> Result<()> {
        self.peripheral
            .write(&self.write_char, data, WriteType::WithResponse)
            .await?;
        Ok(())
    }

    fn subscribe(&self) -> broadcast::Receiver<Vec<u8>> {
        self.tx.subscribe()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn accepts_service_uuid() {
        assert!(is_oura_advertisement(
            &[protocol::OURA_SERVICE],
            &HashMap::new(),
            ""
        ));
    }

    #[test]
    fn accepts_oura_company_id_without_service_list() {
        let mut mfr = HashMap::new();
        mfr.insert(OURA_COMPANY_ID, vec![0x04, 0x40, 0x5c, 0x06]);
        assert!(is_oura_advertisement(&[], &mfr, ""));
    }

    #[test]
    fn name_alone_is_not_enough() {
        assert!(!is_oura_advertisement(
            &[],
            &HashMap::new(),
            "Oura Ring Gen3"
        ));
        assert!(!is_oura_advertisement(&[], &HashMap::new(), "courageous"));
    }

    #[test]
    fn empty_name_needle_matches_everything() {
        assert!(name_filter_matches("", ""));
        assert!(name_filter_matches("", "Oura Ring Gen3"));
        assert!(name_filter_matches("  ", "x"));
    }

    #[test]
    fn oura_needle_allows_empty_winrt_name() {
        assert!(name_filter_matches("Oura", ""));
        assert!(name_filter_matches("oura", "Oura 2H4C…"));
        assert!(!name_filter_matches("Gen3", ""));
        assert!(name_filter_matches("Gen3", "Oura Ring Gen3"));
    }
}
