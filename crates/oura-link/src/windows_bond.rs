//! Windows OS bonding for the ring's encrypted link (feature `ble`, Windows only).
//!
//! A factory-reset ring rejects GATT writes and notification subscriptions until
//! the link is encrypted. Windows Settings pairing is unreliable for the ring, so
//! [`crate::ble::BleTransport::connect`] calls [`ensure_bond`] before btleplug opens
//! GATT. It requests the bond through WinRT custom pairing and accepts only an
//! encrypted result.

use std::future::{Future, IntoFuture};
use std::time::Duration;

use windows::core::Ref;
use windows::Devices::Bluetooth::BluetoothLEDevice;
use windows::Devices::Enumeration::{
    DeviceInformation, DeviceInformationCustomPairing, DeviceInformationPairing,
    DevicePairingKinds, DevicePairingProtectionLevel, DevicePairingRequestedEventArgs,
    DevicePairingResult, DevicePairingResultStatus, DeviceUnpairingResultStatus,
};
use windows::Foundation::TypedEventHandler;

use crate::error::{Error, Result};

/// The only ceremony accepted without the user. The ring has no display or keypad,
/// so it never asks for a PIN.
const PAIRING_KINDS: DevicePairingKinds = DevicePairingKinds::ConfirmOnly;

const OPEN_TIMEOUT: Duration = Duration::from_secs(15);
const UNPAIR_TIMEOUT: Duration = Duration::from_secs(15);
/// Per pairing attempt. A stalled OS ceremony is cancelled rather than left running.
const PAIR_TIMEOUT: Duration = Duration::from_secs(45);
/// How long to wait for a cancelled operation to report how it ended.
const CANCEL_CLEANUP_TIMEOUT: Duration = Duration::from_secs(5);

/// Ensure Windows holds an encrypted bond with the ring at `peripheral_id`.
///
/// Keeps an existing encrypted bond. Replaces a bond without encryption, which
/// Windows reports as paired even though encrypted GATT then fails. Never prints
/// the ring's address or name.
pub(crate) async fn ensure_bond(peripheral_id: &str) -> Result<()> {
    let address = bluetooth_address(peripheral_id)?;

    let open =
        BluetoothLEDevice::FromBluetoothAddressAsync(address).map_err(winrt("open LE device"))?;
    let device = wait_operation(
        "windows bond: open LE device",
        open.clone().into_future(),
        || open.Cancel(),
        OPEN_TIMEOUT,
        CANCEL_CLEANUP_TIMEOUT,
    )
    .await?;
    let _close_device = OnDrop::new(|| {
        let _ = device.Close();
    });

    let mut pairing = device
        .DeviceInformation()
        .and_then(|info| info.Pairing())
        .map_err(winrt("read pairing state"))?;
    if pairing.IsPaired().map_err(winrt("IsPaired"))? {
        let level = pairing
            .ProtectionLevel()
            .map_err(winrt("ProtectionLevel"))?;
        if is_encrypted(level) {
            return Ok(());
        }
        eprintln!(
            "Existing Windows pairing lacks encryption (protection={}); re-pairing...",
            level_name(level)
        );
        unpair(&pairing).await?;
        // The snapshot still describes the removed bond.
        pairing = current_pairing(&device).await?;
    }
    if !pairing.CanPair().map_err(winrt("CanPair"))? {
        return Err(Error::Ble(
            "windows bond: device reports CanPair=false".into(),
        ));
    }

    eprintln!("Requesting Windows BLE bond (approve any OS prompt)...");
    // Never request Default or None: Windows reports those bonds as paired, but
    // the ring still rejects GATT access.
    let mut last = DevicePairingResultStatus::Failed;
    for requested in [
        DevicePairingProtectionLevel::Encryption,
        DevicePairingProtectionLevel::EncryptionAndAuthentication,
    ] {
        let result = pair_once(&pairing, requested).await?;
        last = result.Status().map_err(winrt("PairAsync status"))?;
        if last == DevicePairingResultStatus::Paired
            || last == DevicePairingResultStatus::AlreadyPaired
        {
            // `pairing` still holds the state from before PairAsync, so read the
            // new bond afresh.
            let bond = current_pairing(&device).await?;
            let used = achieved_protection(
                result.ProtectionLevelUsed().ok(),
                bond.ProtectionLevel().map_err(winrt("ProtectionLevel"))?,
            );
            if is_encrypted(used) {
                eprintln!(
                    "Windows BLE bond established (protection={}).",
                    level_name(used)
                );
                return Ok(());
            }
            eprintln!(
                "Windows paired without encryption (protection={}); retrying...",
                level_name(used)
            );
            unpair(&bond).await?;
            pairing = current_pairing(&device).await?;
            continue;
        }
        if !is_retryable(last) {
            break;
        }
    }
    Err(Error::Ble(format!(
        "windows bond: PairAsync status={} (encryption required)",
        status_name(last)
    )))
}

/// One custom pairing attempt at `level` that accepts only the confirm-only
/// ceremony.
async fn pair_once(
    pairing: &DeviceInformationPairing,
    level: DevicePairingProtectionLevel,
) -> Result<DevicePairingResult> {
    let custom = pairing.Custom().map_err(winrt("custom pairing"))?;
    let handler = TypedEventHandler::new(
        |_: Ref<DeviceInformationCustomPairing>, args: Ref<DevicePairingRequestedEventArgs>| {
            if let Ok(args) = args.ok() {
                if should_accept(args.PairingKind()?) {
                    args.Accept()?;
                }
            }
            Ok(())
        },
    );
    let token = custom
        .PairingRequested(&handler)
        .map_err(winrt("register pairing handler"))?;
    let _remove_handler = OnDrop::new(|| {
        let _ = custom.RemovePairingRequested(token);
    });
    let pair = custom
        .PairWithProtectionLevelAsync(PAIRING_KINDS, level)
        .map_err(winrt("start PairAsync"))?;
    wait_operation(
        "windows bond: PairAsync",
        pair.clone().into_future(),
        || pair.Cancel(),
        PAIR_TIMEOUT,
        CANCEL_CLEANUP_TIMEOUT,
    )
    .await
}

async fn unpair(pairing: &DeviceInformationPairing) -> Result<()> {
    let op = pairing.UnpairAsync().map_err(winrt("start UnpairAsync"))?;
    let result = wait_operation(
        "windows bond: UnpairAsync",
        op.clone().into_future(),
        || op.Cancel(),
        UNPAIR_TIMEOUT,
        CANCEL_CLEANUP_TIMEOUT,
    )
    .await?;
    match result.Status().map_err(winrt("UnpairAsync status"))? {
        DeviceUnpairingResultStatus::Unpaired | DeviceUnpairingResultStatus::AlreadyUnpaired => {
            Ok(())
        }
        DeviceUnpairingResultStatus::OperationAlreadyInProgress => {
            Err(unpair_failed("OperationAlreadyInProgress"))
        }
        DeviceUnpairingResultStatus::AccessDenied => Err(unpair_failed("AccessDenied")),
        _ => Err(unpair_failed("Failed")),
    }
}

fn unpair_failed(status: &str) -> Error {
    Error::Ble(format!(
        "windows bond: could not remove the unencrypted pairing (UnpairAsync status={status})"
    ))
}

/// The protection level the new bond ended up with.
///
/// `ProtectionLevelUsed` can report `None` for a bond whose link is encrypted
/// (seen on Windows 11 with a Gen3 ring), and removing that bond to retry fails.
/// Either the result or the bond's current level reporting encryption is enough.
fn achieved_protection(
    used: Option<DevicePairingProtectionLevel>,
    current: DevicePairingProtectionLevel,
) -> DevicePairingProtectionLevel {
    match used {
        Some(level) if is_encrypted(level) => level,
        _ => current,
    }
}

/// Reads the pairing state afresh. `BluetoothLEDevice::DeviceInformation` is a
/// snapshot from when the device was opened and does not see a new bond.
async fn current_pairing(device: &BluetoothLEDevice) -> Result<DeviceInformationPairing> {
    let id = device.DeviceId().map_err(winrt("DeviceId"))?;
    let read =
        DeviceInformation::CreateFromIdAsync(&id).map_err(winrt("read device information"))?;
    let info = wait_operation(
        "windows bond: read device information",
        read.clone().into_future(),
        || read.Cancel(),
        OPEN_TIMEOUT,
        CANCEL_CLEANUP_TIMEOUT,
    )
    .await?;
    info.Pairing().map_err(winrt("read pairing state"))
}

/// Parse a btleplug Windows peripheral id (`AA:BB:CC:DD:EE:FF`) into the 48-bit
/// address WinRT expects.
fn bluetooth_address(peripheral_id: &str) -> Result<u64> {
    let hex: String = peripheral_id
        .chars()
        .filter(char::is_ascii_hexdigit)
        .collect();
    if hex.len() != 12 {
        return Err(Error::Ble(
            "windows bond: peripheral id is not a 6-byte BLE address".into(),
        ));
    }
    u64::from_str_radix(&hex, 16)
        .map_err(|_| Error::Ble("windows bond: invalid BLE address".into()))
}

fn should_accept(kind: DevicePairingKinds) -> bool {
    kind == PAIRING_KINDS
}

fn is_encrypted(level: DevicePairingProtectionLevel) -> bool {
    level == DevicePairingProtectionLevel::Encryption
        || level == DevicePairingProtectionLevel::EncryptionAndAuthentication
}

/// Statuses after which the stronger protection level is still worth trying.
fn is_retryable(status: DevicePairingResultStatus) -> bool {
    status == DevicePairingResultStatus::ProtectionLevelCouldNotBeMet
        || status == DevicePairingResultStatus::Failed
        || status == DevicePairingResultStatus::NotReadyToPair
}

fn winrt(context: &'static str) -> impl FnOnce(windows::core::Error) -> Error {
    move |e| Error::Ble(format!("windows bond: {context}: {e}"))
}

/// Await a WinRT operation for at most `timeout`.
///
/// Dropping the Rust future does not stop the OS operation, so `cancel` runs on a
/// timeout and when the caller drops this future early (for example on its own
/// overall deadline). After a timeout, waits up to `cleanup` for the operation to
/// settle, because a late success may already have changed the bond.
async fn wait_operation<T>(
    label: &str,
    operation: impl Future<Output = windows::core::Result<T>>,
    cancel: impl Fn() -> windows::core::Result<()>,
    timeout: Duration,
    cleanup: Duration,
) -> Result<T> {
    let mut operation = std::pin::pin!(operation);
    let cancel_on_drop = OnDrop::new(|| {
        let _ = cancel();
    });
    let finished = tokio::time::timeout(timeout, operation.as_mut()).await;
    cancel_on_drop.disarm();
    if let Ok(result) = finished {
        return result.map_err(|e| Error::Ble(format!("{label}: {e}")));
    }

    let requested = match cancel() {
        Ok(()) => "cancel requested".to_owned(),
        Err(e) => format!("cancel failed ({e})"),
    };
    let settled = match tokio::time::timeout(cleanup, operation).await {
        Ok(Ok(_)) => "it then completed, so it may have taken effect".to_owned(),
        Ok(Err(e)) => format!("it then ended with: {e}"),
        Err(_) => "completion unconfirmed".to_owned(),
    };
    Err(Error::Ble(format!(
        "{label}: timed out after {timeout:?}; {requested}; {settled}"
    )))
}

/// Runs a closure when dropped unless disarmed.
struct OnDrop<F: FnOnce()>(Option<F>);

impl<F: FnOnce()> OnDrop<F> {
    fn new(f: F) -> Self {
        Self(Some(f))
    }

    fn disarm(mut self) {
        self.0 = None;
    }
}

impl<F: FnOnce()> Drop for OnDrop<F> {
    fn drop(&mut self) {
        if let Some(f) = self.0.take() {
            f();
        }
    }
}

fn level_name(level: DevicePairingProtectionLevel) -> &'static str {
    match level {
        DevicePairingProtectionLevel::Default => "Default",
        DevicePairingProtectionLevel::None => "None",
        DevicePairingProtectionLevel::Encryption => "Encryption",
        DevicePairingProtectionLevel::EncryptionAndAuthentication => "EncryptionAndAuthentication",
        _ => "Unknown",
    }
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
        DevicePairingResultStatus::RequiredHandlerNotRegistered => "RequiredHandlerNotRegistered",
        DevicePairingResultStatus::RejectedByHandler => "RejectedByHandler",
        DevicePairingResultStatus::RemoteDeviceHasAssociation => "RemoteDeviceHasAssociation",
        DevicePairingResultStatus::Failed => "Failed",
        _ => "Unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::task::Poll;

    const SHORT: Duration = Duration::from_millis(5);
    const CLEANUP: Duration = Duration::from_millis(50);

    fn failure() -> windows::core::Error {
        windows::core::Error::from_hresult(windows::core::HRESULT(0x8000_4005_u32 as i32))
    }

    #[test]
    fn parses_btleplug_peripheral_ids() {
        assert_eq!(
            bluetooth_address("A0:38:F8:01:02:0f").unwrap(),
            0xA038_F801_020F
        );
        assert!(bluetooth_address("A0:38:F8:01:02").is_err());
        assert!(bluetooth_address("not an address").is_err());
    }

    #[test]
    fn accepts_only_confirm_only_ceremonies() {
        assert!(should_accept(DevicePairingKinds::ConfirmOnly));
        for kind in [
            DevicePairingKinds::None,
            DevicePairingKinds::DisplayPin,
            DevicePairingKinds::ProvidePin,
            DevicePairingKinds::ConfirmPinMatch,
            DevicePairingKinds::ConfirmOnly | DevicePairingKinds::ProvidePin,
        ] {
            assert!(!should_accept(kind));
        }
    }

    #[test]
    fn either_report_of_encryption_counts() {
        use DevicePairingProtectionLevel as L;
        assert_eq!(
            achieved_protection(Some(L::Encryption), L::None),
            L::Encryption
        );
        assert_eq!(
            achieved_protection(Some(L::None), L::Encryption),
            L::Encryption
        );
        assert_eq!(
            achieved_protection(Some(L::Default), L::Encryption),
            L::Encryption
        );
        assert_eq!(achieved_protection(None, L::Encryption), L::Encryption);
        assert_eq!(achieved_protection(Some(L::None), L::None), L::None);
        assert_eq!(achieved_protection(None, L::Default), L::Default);
    }

    #[test]
    fn only_encrypted_levels_count_as_bonded() {
        assert!(is_encrypted(DevicePairingProtectionLevel::Encryption));
        assert!(is_encrypted(
            DevicePairingProtectionLevel::EncryptionAndAuthentication
        ));
        assert!(!is_encrypted(DevicePairingProtectionLevel::None));
        assert!(!is_encrypted(DevicePairingProtectionLevel::Default));
    }

    #[tokio::test]
    async fn success_never_cancels() {
        let cancels = AtomicUsize::new(0);
        let result = wait_operation(
            "op",
            async { Ok(42) },
            || {
                cancels.fetch_add(1, Ordering::SeqCst);
                Ok(())
            },
            SHORT,
            CLEANUP,
        )
        .await;
        assert_eq!(result.unwrap(), 42);
        assert_eq!(cancels.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn timeout_cancels_and_reports_a_late_completion() {
        let cancelled = AtomicBool::new(false);
        let operation = std::future::poll_fn(|cx| {
            if cancelled.load(Ordering::SeqCst) {
                Poll::Ready(Ok(7))
            } else {
                cx.waker().wake_by_ref();
                Poll::Pending
            }
        });
        let error = wait_operation(
            "op",
            operation,
            || {
                cancelled.store(true, Ordering::SeqCst);
                Ok(())
            },
            SHORT,
            CLEANUP,
        )
        .await
        .unwrap_err()
        .to_string();
        assert!(error.contains("cancel requested"), "{error}");
        assert!(error.contains("may have taken effect"), "{error}");
    }

    #[tokio::test]
    async fn failed_cancel_and_terminal_error_are_both_reported() {
        let cancelled = AtomicBool::new(false);
        let operation = std::future::poll_fn(|cx| {
            if cancelled.load(Ordering::SeqCst) {
                Poll::Ready(Err::<u32, _>(failure()))
            } else {
                cx.waker().wake_by_ref();
                Poll::Pending
            }
        });
        let error = wait_operation(
            "op",
            operation,
            || {
                cancelled.store(true, Ordering::SeqCst);
                Err(failure())
            },
            SHORT,
            CLEANUP,
        )
        .await
        .unwrap_err()
        .to_string();
        assert!(error.contains("cancel failed"), "{error}");
        assert!(error.contains("then ended with"), "{error}");
    }

    #[tokio::test]
    async fn operation_that_never_settles_is_reported_unconfirmed() {
        let error = wait_operation(
            "op",
            std::future::pending::<windows::core::Result<u32>>(),
            || Ok(()),
            SHORT,
            CLEANUP,
        )
        .await
        .unwrap_err()
        .to_string();
        assert!(error.contains("completion unconfirmed"), "{error}");
    }

    #[tokio::test]
    async fn dropping_the_wait_cancels_once() {
        let cancels = Arc::new(AtomicUsize::new(0));
        let counter = cancels.clone();
        let wait = wait_operation(
            "op",
            std::future::pending::<windows::core::Result<u32>>(),
            move || {
                counter.fetch_add(1, Ordering::SeqCst);
                Ok(())
            },
            Duration::from_secs(60),
            CLEANUP,
        );
        assert!(tokio::time::timeout(SHORT, wait).await.is_err());
        assert_eq!(cancels.load(Ordering::SeqCst), 1);
    }
}
