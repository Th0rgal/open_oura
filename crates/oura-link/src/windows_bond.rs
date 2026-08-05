//! Windows OS-level BLE bonding (link encryption) via WinRT custom PairAsync.
//!
//! Factory-reset Oura rings require encrypted ATT before notify/write. On
//! Windows, Settings UI pairing often fails; requesting a bond through
//! `DeviceInformationCustomPairing` before GATT access is the reliable path.

use crate::error::{Error, Result};
use std::future::IntoFuture;
use std::time::Duration;
use windows::core::{HSTRING, Ref};
use windows::Devices::Bluetooth::BluetoothLEDevice;
use windows::Devices::Enumeration::{
    DeviceInformationCustomPairing, DeviceInformationPairing, DevicePairingKinds,
    DevicePairingProtectionLevel, DevicePairingRequestedEventArgs, DevicePairingResult,
    DevicePairingResultStatus, DeviceUnpairingResultStatus,
};
use windows::Foundation::TypedEventHandler;

/// Parse a btleplug Windows peripheral id (`AA:BB:CC:DD:EE:FF`) into a WinRT
/// Bluetooth address integer. Never logs the address.
fn bluetooth_address_from_id(peripheral_id: &str) -> Result<u64> {
    let hex: String = peripheral_id
        .chars()
        .filter(|c| c.is_ascii_hexdigit())
        .collect();
    if hex.len() != 12 {
        return Err(Error::Ble(
            "windows bond: peripheral id is not a 6-byte BLE address".into(),
        ));
    }
    u64::from_str_radix(&hex, 16)
        .map_err(|_| Error::Ble("windows bond: invalid BLE address hex".into()))
}

fn status_name(status: DevicePairingResultStatus) -> &'static str {
    match status {
        DevicePairingResultStatus::Paired => "Paired",
        DevicePairingResultStatus::NotReadyToPair => "NotReadyToPair",
        DevicePairingResultStatus::NotPaired => "NotPaired",
        DevicePairingResultStatus::AlreadyPaired => "AlreadyPaired",
        DevicePairingResultStatus::ConnectionRejected => "ConnectionRejected",
        DevicePairingResultStatus::TooManyConnections => "TooManyConnections",
        DevicePairingResultStatus::HardwareFailure => "HardwareFailure",
        DevicePairingResultStatus::AuthenticationTimeout => "AuthenticationTimeout",
        DevicePairingResultStatus::AuthenticationNotAllowed => "AuthenticationNotAllowed",
        DevicePairingResultStatus::AuthenticationFailure => "AuthenticationFailure",
        DevicePairingResultStatus::NoSupportedProfiles => "NoSupportedProfiles",
        DevicePairingResultStatus::ProtectionLevelCouldNotBeMet => "ProtectionLevelCouldNotBeMet",
        DevicePairingResultStatus::AccessDenied => "AccessDenied",
        DevicePairingResultStatus::InvalidCeremonyData => "InvalidCeremonyData",
        DevicePairingResultStatus::PairingCanceled => "PairingCanceled",
        DevicePairingResultStatus::OperationAlreadyInProgress => "OperationAlreadyInProgress",
        DevicePairingResultStatus::RequiredHandlerNotRegistered => {
            "RequiredHandlerNotRegistered"
        }
        DevicePairingResultStatus::RejectedByHandler => "RejectedByHandler",
        DevicePairingResultStatus::RemoteDeviceHasAssociation => "RemoteDeviceHasAssociation",
        DevicePairingResultStatus::Failed => "Failed",
        _ => "Unknown",
    }
}

fn level_name(level: DevicePairingProtectionLevel) -> &'static str {
    match level {
        DevicePairingProtectionLevel::Default => "Default",
        DevicePairingProtectionLevel::None => "None",
        DevicePairingProtectionLevel::Encryption => "Encryption",
        DevicePairingProtectionLevel::EncryptionAndAuthentication => {
            "EncryptionAndAuthentication"
        }
        _ => "Unknown",
    }
}

fn is_encrypted(level: DevicePairingProtectionLevel) -> bool {
    level == DevicePairingProtectionLevel::Encryption
        || level == DevicePairingProtectionLevel::EncryptionAndAuthentication
}

fn pairing_kinds() -> DevicePairingKinds {
    DevicePairingKinds::ConfirmOnly
        | DevicePairingKinds::DisplayPin
        | DevicePairingKinds::ProvidePin
        | DevicePairingKinds::ConfirmPinMatch
}

async fn unpair_with_timeout(
    pairing: &DeviceInformationPairing,
    timeout: Duration,
) -> Result<()> {
    let op = pairing
        .UnpairAsync()
        .map_err(|e| Error::Ble(format!("windows bond: UnpairAsync start failed: {e}")))?;
    let cancel_handle = op.clone();
    let unpair = match tokio::time::timeout(timeout, op.into_future()).await {
        Ok(Ok(r)) => r,
        Ok(Err(e)) => {
            return Err(Error::Ble(format!("windows bond: UnpairAsync failed: {e}")));
        }
        Err(_elapsed) => {
            let _ = cancel_handle.Cancel();
            return Err(Error::Ble(
                "windows bond: UnpairAsync timed out (OS ceremony cancelled)".into(),
            ));
        }
    };
    let ustatus = unpair
        .Status()
        .map_err(|e| Error::Ble(format!("windows bond: UnpairAsync status failed: {e}")))?;
    if ustatus != DeviceUnpairingResultStatus::Unpaired
        && ustatus != DeviceUnpairingResultStatus::AlreadyUnpaired
    {
        return Err(Error::Ble(format!(
            "windows bond: UnpairAsync status={}",
            match ustatus {
                DeviceUnpairingResultStatus::OperationAlreadyInProgress => {
                    "OperationAlreadyInProgress"
                }
                DeviceUnpairingResultStatus::AccessDenied => "AccessDenied",
                DeviceUnpairingResultStatus::Failed => "Failed",
                _ => "Unknown",
            }
        )));
    }
    Ok(())
}

fn achieved_protection(
    result: &DevicePairingResult,
    pairing: &windows::Devices::Enumeration::DeviceInformationPairing,
) -> Result<DevicePairingProtectionLevel> {
    // Prefer the result's ProtectionLevelUsed; fall back to the association's
    // current ProtectionLevel if the result does not expose one.
    if let Ok(used) = result.ProtectionLevelUsed() {
        if used != DevicePairingProtectionLevel::Default {
            return Ok(used);
        }
    }
    pairing
        .ProtectionLevel()
        .map_err(|e| Error::Ble(format!("windows bond: ProtectionLevel failed: {e}")))
}

/// Ensure the OS has a BLE bond with encryption for `peripheral_id`.
///
/// Uses custom pairing with a PairingRequested handler (required on Windows for
/// Just Works / confirm-only BLE bonds). Does not print addresses or names.
/// Only Encryption / EncryptionAndAuthentication bonds are accepted.
pub async fn ensure_ble_bond(peripheral_id: &str) -> Result<()> {
    let address = bluetooth_address_from_id(peripheral_id)?;
    let kinds = pairing_kinds();

    // Helps console hosts receive inbound pairing ceremonies.
    let _ = DeviceInformationPairing::TryRegisterForAllInboundPairingRequests(kinds);

    let device = BluetoothLEDevice::FromBluetoothAddressAsync(address)
        .map_err(|e| Error::Ble(format!("windows bond: open LE device failed: {e}")))?
        .into_future()
        .await
        .map_err(|e| Error::Ble(format!("windows bond: open LE device failed: {e}")))?;

    let info = device
        .DeviceInformation()
        .map_err(|e| Error::Ble(format!("windows bond: DeviceInformation failed: {e}")))?;
    let pairing = info
        .Pairing()
        .map_err(|e| Error::Ble(format!("windows bond: Pairing API failed: {e}")))?;

    if pairing
        .IsPaired()
        .map_err(|e| Error::Ble(format!("windows bond: IsPaired failed: {e}")))?
    {
        let level = pairing
            .ProtectionLevel()
            .map_err(|e| Error::Ble(format!("windows bond: ProtectionLevel failed: {e}")))?;
        // Stale Settings pairings can report IsPaired with Default/None and still
        // fail encrypted GATT. Require at least Encryption.
        if is_encrypted(level) {
            let _ = device.Close();
            return Ok(());
        }
        eprintln!(
            "Existing Windows pairing lacks encryption (level={}); re-pairing...",
            level_name(level)
        );
        if let Err(e) = unpair_with_timeout(&pairing, Duration::from_secs(15)).await {
            let _ = device.Close();
            return Err(e);
        }
    }

    if !pairing
        .CanPair()
        .map_err(|e| Error::Ble(format!("windows bond: CanPair failed: {e}")))?
    {
        let _ = device.Close();
        return Err(Error::Ble(
            "windows bond: device reports CanPair=false".into(),
        ));
    }

    let custom = pairing
        .Custom()
        .map_err(|e| Error::Ble(format!("windows bond: Custom pairing API failed: {e}")))?;

    let handler = TypedEventHandler::new(
        move |_sender: Ref<DeviceInformationCustomPairing>,
              args: Ref<DevicePairingRequestedEventArgs>| {
            if let Ok(args) = args.ok() {
                let kind = args.PairingKind()?;
                if kind == DevicePairingKinds::ConfirmOnly
                    || kind == DevicePairingKinds::DisplayPin
                    || kind == DevicePairingKinds::ConfirmPinMatch
                {
                    args.Accept()?;
                } else if kind == DevicePairingKinds::ProvidePin {
                    // Common Just Works fallback when the peer asks for a PIN.
                    args.AcceptWithPin(&HSTRING::from("0000"))?;
                }
            }
            Ok(())
        },
    );
    let token = custom
        .PairingRequested(&handler)
        .map_err(|e| Error::Ble(format!("windows bond: PairingRequested register failed: {e}")))?;

    eprintln!("Requesting Windows BLE bond (approve any OS prompt)...");

    // Never fall back to Default/None: those associations report Paired but do
    // not satisfy encrypted GATT the ring requires.
    let levels = [
        DevicePairingProtectionLevel::Encryption,
        DevicePairingProtectionLevel::EncryptionAndAuthentication,
    ];

    // Per-attempt budget so a hung OS ceremony is Cancel()'d rather than only
    // dropping the Rust future (which would leave PairAsync running).
    const PAIR_ATTEMPT_TIMEOUT: Duration = Duration::from_secs(45);

    let mut last = DevicePairingResultStatus::Failed;
    for level in levels {
        let op = custom
            .PairWithProtectionLevelAsync(kinds, level)
            .map_err(|e| Error::Ble(format!("windows bond: PairAsync start failed: {e}")))?;
        let cancel_handle = op.clone();
        let result = match tokio::time::timeout(PAIR_ATTEMPT_TIMEOUT, op.into_future()).await {
            Ok(Ok(r)) => r,
            Ok(Err(e)) => {
                let _ = custom.RemovePairingRequested(token);
                let _ = device.Close();
                return Err(Error::Ble(format!("windows bond: PairAsync failed: {e}")));
            }
            Err(_elapsed) => {
                // Cancel the WinRT op — dropping the future alone leaves the OS
                // pairing ceremony running and can race the next attempt.
                let _ = cancel_handle.Cancel();
                let _ = custom.RemovePairingRequested(token);
                let _ = device.Close();
                return Err(Error::Ble(
                    "windows bond: PairAsync timed out (OS ceremony cancelled)".into(),
                ));
            }
        };
        last = result
            .Status()
            .map_err(|e| Error::Ble(format!("windows bond: PairAsync status failed: {e}")))?;
        if last == DevicePairingResultStatus::Paired
            || last == DevicePairingResultStatus::AlreadyPaired
        {
            let used = achieved_protection(&result, &pairing)?;
            if is_encrypted(used) {
                let _ = custom.RemovePairingRequested(token);
                let _ = device.Close();
                eprintln!(
                    "Windows BLE bond established (protection={}).",
                    level_name(used)
                );
                return Ok(());
            }
            // Paired but weak — tear it down and keep trying stronger levels.
            eprintln!(
                "Windows pairing succeeded at insufficient protection ({}); retrying...",
                level_name(used)
            );
            let _ = unpair_with_timeout(&pairing, Duration::from_secs(15)).await;
            continue;
        }
        if last != DevicePairingResultStatus::ProtectionLevelCouldNotBeMet
            && last != DevicePairingResultStatus::Failed
            && last != DevicePairingResultStatus::NotReadyToPair
        {
            break;
        }
    }

    let _ = custom.RemovePairingRequested(token);
    let _ = device.Close();
    Err(Error::Ble(format!(
        "windows bond: PairAsync status={} (encryption required)",
        status_name(last)
    )))
}
