// Throwaway diagnostic: unfiltered BLE scan. Prints every advertiser seen so we
// can tell "radio works, ring absent" from "radio sees nothing".
use btleplug::api::{Central, Manager as _, Peripheral as _, ScanFilter};
use btleplug::platform::Manager;
use std::collections::HashMap;
use std::time::{Duration, Instant};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let manager = Manager::new().await?;
    let adapter = manager.adapters().await?.into_iter().next().expect("no BLE adapter");
    adapter.start_scan(ScanFilter::default()).await?;
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut seen: HashMap<String, (String, i16)> = HashMap::new();
    while Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(500)).await;
        for p in adapter.peripherals().await? {
            if let Some(props) = p.properties().await? {
                let name = props.local_name.unwrap_or_default();
                let rssi = props.rssi.unwrap_or(i16::MIN);
                let oura = props.services.iter().any(|s| s.to_string().starts_with("98ed0001"))
                    || name.to_lowercase().contains("oura");
                let tag = if oura { format!("*** OURA *** {name}") } else { name };
                seen.insert(p.id().to_string(), (tag, rssi));
            }
        }
    }
    let _ = adapter.stop_scan().await;
    let mut v: Vec<_> = seen.into_iter().collect();
    v.sort_by_key(|(_, (_, r))| std::cmp::Reverse(*r));
    println!("== {} distinct advertisers in 20s ==", v.len());
    for (id, (name, rssi)) in v {
        println!("  rssi={rssi:>5}  {:<28}  {}", if name.is_empty() { "<no name>".into() } else { name }, &id[..id.len().min(20)]);
    }
    Ok(())
}
