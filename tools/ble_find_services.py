#!/usr/bin/env python3
"""Find a BLE device by advertised service/name and enumerate GATT services."""

import argparse
import asyncio

from bleak import BleakClient, BleakScanner


async def main() -> None:
    parser = argparse.ArgumentParser(description="Find and enumerate a BLE device.")
    parser.add_argument("--service", help="Advertised service UUID to match")
    parser.add_argument("--name-contains", help="Substring to match in advertised name")
    parser.add_argument("--scan-timeout", type=float, default=20.0)
    parser.add_argument("--connect-timeout", type=float, default=30.0)
    args = parser.parse_args()

    service = args.service.lower() if args.service else None
    name_contains = args.name_contains.lower() if args.name_contains else None

    devices = await BleakScanner.discover(timeout=args.scan_timeout, return_adv=True)
    candidates = []
    for device, adv in devices.values():
        name = device.name or adv.local_name or ""
        service_uuids = [uuid.lower() for uuid in adv.service_uuids or []]
        service_match = service and service in service_uuids
        name_match = name_contains and name_contains in name.lower()
        if service_match or name_match:
            candidates.append((device, adv))

    if not candidates:
        print("no_candidates")
        return

    for index, (device, adv) in enumerate(candidates):
        name = device.name or adv.local_name or ""
        print(
            f"candidate[{index}] address={device.address} rssi={adv.rssi} "
            f"name={name!r} services={','.join(adv.service_uuids or [])}"
        )

    device, _adv = candidates[0]
    print(f"connecting address={device.address}")
    async with BleakClient(device, timeout=args.connect_timeout) as client:
        print(f"connected={client.is_connected}")
        try:
            print(f"mtu_size={client.mtu_size}")
        except Exception:
            pass

        for gatt_service in client.services:
            print(f"service uuid={gatt_service.uuid} handle={gatt_service.handle}")
            for char in gatt_service.characteristics:
                props = ",".join(char.properties)
                print(
                    f"  characteristic uuid={char.uuid} handle={char.handle} "
                    f"properties={props}"
                )
                for descriptor in char.descriptors:
                    print(
                        f"    descriptor uuid={descriptor.uuid} "
                        f"handle={descriptor.handle}"
                    )


if __name__ == "__main__":
    asyncio.run(main())
