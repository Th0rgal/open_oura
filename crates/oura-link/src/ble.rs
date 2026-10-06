//! `btleplug`-backed [`Transport`] for real rings (feature `ble`).

use std::time::{Duration, Instant};

use btleplug::api::{
    Central, CharPropFlags, Characteristic, Manager as _, Peripheral as _, PeripheralProperties,
    ScanFilter, WriteType,
};
use btleplug::platform::{Manager, Peripheral};
use futures::StreamExt;
use tokio::sync::broadcast;

use crate::error::{Error, Result};
use crate::transport::Transport;
use oura_protocol::protocol;

/// Upper bound on the BLE connect + service-discovery + subscribe phase. CoreBluetooth
/// imposes no deadline itself, so without this a ring that won't complete the GATT
/// handshake hangs the caller forever.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

/// Bluetooth SIG company identifier of Oura Health Oy, carried in the ring's
/// manufacturer-specific advertising data.
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

/// The OS scan filter. Rings can list the Oura service as an incomplete 128-bit
/// UUID list, and a WinRT service filter can hide those advertisements, so Windows
/// scans unfiltered and relies on [`is_oura_ring`] instead.
fn scan_filter() -> ScanFilter {
    if cfg!(windows) {
        ScanFilter::default()
    } else {
        ScanFilter {
            services: vec![protocol::OURA_SERVICE],
        }
    }
}

/// Whether an advertisement comes from an Oura ring whose name matches `--name`.
///
/// The advertisement must carry the Oura service UUID or Oura's company ID in
/// its manufacturer data. WinRT often drops the service UUID, so the company ID
/// is what identifies the ring there. A name is never enough on its own: on
/// Windows, connecting starts an OS bond before GATT can confirm the device, so
/// an unrelated device named "...oura..." must not be selected.
fn is_oura_ring(props: &PeripheralProperties, name_contains: &str) -> bool {
    let oura = props.services.contains(&protocol::OURA_SERVICE)
        || props.manufacturer_data.contains_key(&OURA_COMPANY_ID);
    oura && name_matches(props.local_name.as_deref().unwrap_or(""), name_contains)
}

/// Whether an advertised local name matches `--name`.
///
/// Scan already requires Oura evidence (see [`is_oura_ring`]), so the name is
/// only a secondary filter. A bonded ring often stops advertising a local name;
/// treating that as a miss (`"".contains("oura") == false`) is
/// https://github.com/Th0rgal/open_oura/issues/13.
/// Empty names therefore pass the default `"Oura"` needle. A more specific needle
/// still requires a name, so `--name "Ring 5"` does not pick every unnamed device.
pub fn name_matches(advertised: &str, needle: &str) -> bool {
    let needle = needle.trim().to_lowercase();
    if needle.is_empty() {
        return true;
    }
    if advertised.is_empty() {
        return needle == "oura";
    }
    advertised.to_lowercase().contains(&needle)
}

/// Scan for Oura rings (see [`is_oura_ring`]), filtered by case-insensitive name
/// substring. Returns candidates sorted by signal strength (strongest first).
pub async fn scan(name_contains: &str, timeout: Duration) -> Result<Vec<Discovered>> {
    let adapter = first_adapter().await?;
    adapter.start_scan(scan_filter()).await?;

    let deadline = Instant::now() + timeout;
    let mut found: Vec<Discovered> = Vec::new();
    while Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(400)).await;
        for p in adapter.peripherals().await? {
            let Some(props) = p.properties().await? else {
                continue;
            };
            if !is_oura_ring(&props, name_contains) {
                continue;
            }
            let name = props.local_name.unwrap_or_default();
            let id = p.id().to_string();
            let entry = Discovered {
                id: id.clone(),
                name: if name.is_empty() {
                    "(unnamed)".into()
                } else {
                    name
                },
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
        adapter.start_scan(scan_filter()).await?;

        let deadline = Instant::now() + scan_timeout;
        let mut chosen: Option<(Peripheral, i16)> = None;
        while Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(400)).await;
            for p in adapter.peripherals().await? {
                let Some(props) = p.properties().await? else {
                    continue;
                };
                if !is_oura_ring(&props, name_contains) {
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

    fn advertisement(
        name: Option<&str>,
        service: bool,
        company_id: Option<u16>,
    ) -> PeripheralProperties {
        let mut props = PeripheralProperties {
            local_name: name.map(str::to_owned),
            ..PeripheralProperties::default()
        };
        if service {
            props.services.push(protocol::OURA_SERVICE);
        }
        if let Some(id) = company_id {
            props.manufacturer_data.insert(id, vec![0x04, 0x40]);
        }
        props
    }

    #[test]
    fn service_uuid_or_company_id_identifies_a_ring() {
        let by_service = advertisement(Some("Oura Ring 4"), true, None);
        let by_company = advertisement(None, false, Some(OURA_COMPANY_ID));
        assert!(is_oura_ring(&by_service, "Oura"));
        assert!(is_oura_ring(&by_company, "Oura"));
        assert!(is_oura_ring(&by_company, ""));
    }

    #[test]
    fn name_alone_never_identifies_a_ring() {
        let named = advertisement(Some("Oura Ring Gen3"), false, None);
        let other_company = advertisement(Some("Oura Ring Gen3"), false, Some(0x004C));
        assert!(!is_oura_ring(&named, "Oura"));
        assert!(!is_oura_ring(&other_company, ""));
    }

    #[test]
    fn specific_needle_rejects_an_unnamed_ring() {
        let by_service = advertisement(None, true, None);
        let by_company = advertisement(None, false, Some(OURA_COMPANY_ID));
        assert!(!is_oura_ring(&by_service, "Ring 4"));
        assert!(!is_oura_ring(&by_company, "Ring 5"));
    }

    #[test]
    fn default_needle_accepts_unnamed_bonded_ring() {
        assert!(name_matches("", "Oura"));
        assert!(name_matches("", "oura"));
        assert!(name_matches("Oura Ring 4", "Oura"));
    }

    #[test]
    fn specific_needle_still_requires_a_name() {
        assert!(!name_matches("", "Ring 5"));
        assert!(name_matches("Oura Ring 5", "Ring 5"));
        assert!(!name_matches("Oura Ring 4", "Ring 5"));
    }

    #[test]
    fn empty_needle_matches_everything() {
        assert!(name_matches("", ""));
        assert!(name_matches("anything", "  "));
    }
}
