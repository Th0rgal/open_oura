#!/usr/bin/env python3
"""Send low-risk Oura BLE requests and log notification responses."""

import argparse
import asyncio
from datetime import datetime, timezone

from bleak import BleakClient, BleakScanner
from bleak.exc import BleakError


OURA_SERVICE = "98ed0001-a541-11e4-b6a0-0002a5d5c51b"
OURA_WRITE = "98ed0002-a541-11e4-b6a0-0002a5d5c51b"

REQUESTS = {
    "battery": bytes.fromhex("0c00"),
    "firmware": bytes.fromhex("0803000000"),
    "nonce": bytes.fromhex("2f012b"),
}


def parse_packet(data: bytes) -> str:
    if len(data) < 2:
        return "short_packet"
    tag = data[0]
    length = data[1]
    payload = data[2:]
    status = "ok" if length == len(payload) else f"length_mismatch expected={length} actual={len(payload)}"
    fields = [f"tag=0x{tag:02x}", f"length={length}", f"payload={payload.hex()}", status]
    if tag == 0x0D and len(payload) >= 3:
        fields.append(f"battery_percent={payload[0]}")
        fields.append(f"charging_progress={payload[1]}")
        fields.append(f"charging_recommended={payload[2]}")
    if tag == 0x09 and len(payload) >= 12:
        fields.append(f"api={payload[0]}.{payload[1]}.{payload[2]}")
        fields.append(f"firmware={payload[3]}.{payload[4]}.{payload[5]}")
        fields.append(f"bootloader={payload[6]}.{payload[7]}.{payload[8]}")
        fields.append(f"bt_stack={payload[9]}.{payload[10]}.{payload[11]}")
        if len(payload) >= 18:
            mac = ":".join(f"{byte:02x}" for byte in reversed(payload[12:18]))
            fields.append(f"mac={mac}")
    if tag == 0x2F and len(payload) >= 2 and payload[0] == 0x2F:
        fields.append(f"auth_state=0x{payload[1]:02x}")
    if tag == 0x2F and len(payload) >= 1 and payload[0] == 0x2C:
        fields.append(f"auth_nonce={payload[1:].hex()}")
    return " ".join(fields)


async def main() -> None:
    parser = argparse.ArgumentParser(description="Probe an Oura Ring over BLE.")
    parser.add_argument("request", choices=sorted(REQUESTS))
    parser.add_argument("--address", help="BLE address/platform identifier to target")
    parser.add_argument(
        "--name-contains",
        default="Ring 5",
        help="Case-insensitive device name filter",
    )
    parser.add_argument("--scan-timeout", type=float, default=20.0)
    parser.add_argument("--connect-timeout", type=float, default=45.0)
    parser.add_argument("--listen-seconds", type=float, default=5.0)
    args = parser.parse_args()

    devices = await BleakScanner.discover(timeout=args.scan_timeout, return_adv=True)
    candidates = []
    for device, adv in devices.values():
        if OURA_SERVICE not in [uuid.lower() for uuid in adv.service_uuids or []]:
            continue
        name = device.name or adv.local_name or ""
        if args.address and device.address.lower() != args.address.lower():
            continue
        if args.name_contains and args.name_contains.lower() not in name.lower():
            continue
        candidates.append((device, adv))

    if not candidates:
        print("no_oura_candidates")
        return

    candidates.sort(key=lambda item: item[1].rssi, reverse=True)
    for index, (candidate, candidate_adv) in enumerate(candidates):
        candidate_name = candidate.name or candidate_adv.local_name or ""
        print(
            f"candidate[{index}] address={candidate.address} "
            f"rssi={candidate_adv.rssi} name={candidate_name!r}"
        )

    device, adv = candidates[0]
    name = device.name or adv.local_name or ""
    print(
        f"connecting address={device.address} rssi={adv.rssi} "
        f"name={name!r} request={args.request}"
    )

    responses = []

    def notification_handler(sender, data: bytearray) -> None:
        timestamp = datetime.now(timezone.utc).isoformat()
        payload = bytes(data)
        responses.append((sender, payload))
        print(
            f"notification utc={timestamp} sender={sender} "
            f"hex={payload.hex()} parsed='{parse_packet(payload)}'"
        )

    async with BleakClient(device, timeout=args.connect_timeout) as client:
        print(f"connected={client.is_connected}")
        print(f"mtu_size={client.mtu_size}")

        notify_chars = [
            char
            for service in client.services
            for char in service.characteristics
            if service.uuid.lower() == OURA_SERVICE and "notify" in char.properties
        ]
        active_notify_chars = []
        for char in notify_chars:
            try:
                await client.start_notify(char, notification_handler)
            except BleakError as exc:
                print(f"notify_failed uuid={char.uuid} handle={char.handle} error={exc}")
                continue
            active_notify_chars.append(char)
            print(f"notify_started uuid={char.uuid} handle={char.handle}")

        request = REQUESTS[args.request]
        print(f"write uuid={OURA_WRITE} hex={request.hex()}")
        try:
            await client.write_gatt_char(OURA_WRITE, request, response=True)
        except BleakError as exc:
            print(f"write_failed uuid={OURA_WRITE} error={exc}")
            return
        await asyncio.sleep(args.listen_seconds)

        for char in active_notify_chars:
            await client.stop_notify(char)
            print(f"notify_stopped uuid={char.uuid} handle={char.handle}")

    print(f"response_count={len(responses)}")


if __name__ == "__main__":
    asyncio.run(main())
