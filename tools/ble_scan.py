#!/usr/bin/env python3
"""Scan for nearby BLE devices and print likely Oura candidates."""

import argparse
import asyncio
from datetime import datetime, timezone
from typing import Optional

from bleak import BleakScanner


def looks_like_oura(name: Optional[str]) -> bool:
    if not name:
        return False
    lowered = name.lower()
    return "oura" in lowered or "ring" in lowered


def format_manufacturer_data(data: dict[int, bytes]) -> str:
    return ",".join(f"{company_id:04x}:{payload.hex()}" for company_id, payload in data.items())


def format_service_data(data: dict[str, bytes]) -> str:
    return ",".join(f"{uuid}:{payload.hex()}" for uuid, payload in data.items())


async def main() -> None:
    parser = argparse.ArgumentParser(description="Scan for BLE devices.")
    parser.add_argument("--timeout", type=float, default=10.0)
    args = parser.parse_args()

    started = datetime.now(timezone.utc).isoformat()
    print(f"scan_started_utc={started}")
    devices = await BleakScanner.discover(timeout=args.timeout, return_adv=True)

    for device, adv in sorted(devices.values(), key=lambda item: item[0].address):
        name = device.name or adv.local_name or ""
        marker = "*" if looks_like_oura(name) else " "
        uuids = ",".join(adv.service_uuids or [])
        manufacturer_data = format_manufacturer_data(adv.manufacturer_data)
        service_data = format_service_data(adv.service_data)
        print(
            f"{marker} address={device.address} rssi={adv.rssi} "
            f"name={name!r} services={uuids} "
            f"manufacturer_data={manufacturer_data} service_data={service_data}"
        )


if __name__ == "__main__":
    asyncio.run(main())
