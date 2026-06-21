#!/usr/bin/env python3
"""Find a BLE device and read all characteristics that advertise read support."""

import argparse
import asyncio

from bleak import BleakClient, BleakScanner


async def main() -> None:
    parser = argparse.ArgumentParser(description="Read readable BLE characteristics.")
    parser.add_argument("--service", required=True, help="Advertised service UUID to match")
    parser.add_argument("--scan-timeout", type=float, default=20.0)
    parser.add_argument("--connect-timeout", type=float, default=30.0)
    args = parser.parse_args()

    service = args.service.lower()
    devices = await BleakScanner.discover(timeout=args.scan_timeout, return_adv=True)
    candidates = [
        (device, adv)
        for device, adv in devices.values()
        if service in [uuid.lower() for uuid in adv.service_uuids or []]
    ]

    if not candidates:
        print("no_candidates")
        return

    device, adv = candidates[0]
    name = device.name or adv.local_name or ""
    print(f"connecting address={device.address} rssi={adv.rssi} name={name!r}")

    async with BleakClient(device, timeout=args.connect_timeout) as client:
        print(f"connected={client.is_connected}")
        for gatt_service in client.services:
            for char in gatt_service.characteristics:
                if "read" not in char.properties:
                    continue
                try:
                    value = await client.read_gatt_char(char)
                    print(f"read uuid={char.uuid} handle={char.handle} value={value.hex()}")
                except Exception as exc:
                    print(
                        f"read_failed uuid={char.uuid} handle={char.handle} "
                        f"error={type(exc).__name__}: {exc}"
                    )


if __name__ == "__main__":
    asyncio.run(main())
