//! Unfiltered BLE advertisement scan — a connectivity diagnostic.
//!
//! `oura scan` filters on the ring's service and just says "not found", which
//! can't distinguish the three failure modes that actually happen:
//!   * the radio sees nothing        → Bluetooth off / restricted,
//!   * the radio sees other devices  → the ring's single BLE link is held
//!     elsewhere (a phone running the official app), or it's asleep / flat,
//!   * the ring's charging case shows but the ring doesn't → same as above,
//!     with proof the ring is physically nearby.
//!
//! This lists *every* advertiser (Oura ones flagged) so you can tell which case
//! you're in. Run with: `cargo run -p oura-link --features ble --example rawscan`.
use btleplug::api::{Central, Manager as _, Peripheral as _, ScanFilter};
use btleplug::platform::Manager;
use std::collections::HashMap;
use std::time::{Duration, Instant};

use oura_protocol::protocol::OURA_SERVICE;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let manager = Manager::new().await?;
    let adapter = manager
        .adapters()
        .await?
        .into_iter()
        .next()
        .expect("no BLE adapter");
    adapter.start_scan(ScanFilter::default()).await?;
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut seen: HashMap<String, (String, i16)> = HashMap::new();
    while Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(500)).await;
        for p in adapter.peripherals().await? {
            if let Some(props) = p.properties().await? {
                let name = props.local_name.unwrap_or_default();
                let rssi = props.rssi.unwrap_or(i16::MIN);
                let oura =
                    props.services.contains(&OURA_SERVICE) || name.to_lowercase().contains("oura");
                let tag = if oura {
                    format!("*** OURA *** {name}")
                } else {
                    name
                };
                seen.insert(p.id().to_string(), (tag, rssi));
            }
        }
    }
    let _ = adapter.stop_scan().await;
    let mut v: Vec<_> = seen.into_iter().collect();
    v.sort_by_key(|(_, (_, r))| std::cmp::Reverse(*r));
    println!("== {} distinct advertisers in 20s ==", v.len());
    for (id, (name, rssi)) in v {
        let rssi = if rssi == i16::MIN {
            "  n/a".to_string()
        } else {
            format!("{rssi:>5}")
        };
        println!(
            "  rssi={rssi}  {:<28}  {}",
            if name.is_empty() {
                "<no name>".into()
            } else {
                name
            },
            &id[..id.len().min(20)]
        );
    }
    Ok(())
}
