#!/usr/bin/env python3
"""Pull Oura Android app state from a rooted phone and extract a ring auth key.

The official app stores the BLE app-auth key in its Realm ring configuration
once local onboarding has completed. This helper is intentionally conservative:
it reports pairing state from app logs and writes candidate key material to
0600 files, never printing secrets to stdout.
"""

from __future__ import annotations

import argparse
import os
import re
import subprocess
import sys
from collections import Counter
from pathlib import Path


PKG = "com.ouraring.oura"
SERIAL_RE = re.compile(rb"50[0-9A-Z]{3}B[0-9A-Z]{10}")
HEX_KEY_RE = re.compile(rb"[0-9a-f]{32}")


def shannon_entropy(value: bytes) -> float:
    counts = Counter(value)
    length = len(value)
    return -sum((count / length) * __import__("math").log2(count / length) for count in counts.values())


def looks_like_binary_key(value: bytes) -> bool:
    if len(value) != 16:
        return False
    if len(set(value)) < 10:
        return False
    if value.count(0) > 2:
        return False
    if all(32 <= byte < 127 for byte in value):
        return False
    return shannon_entropy(value) >= 3.4


def run_adb(args: list[str], *, check: bool = True, text: bool = True) -> subprocess.CompletedProcess:
    return subprocess.run(["adb", *args], check=check, text=text, capture_output=True)


def adb_shell(cmd: str, *, check: bool = True) -> str:
    return run_adb(["shell", cmd], check=check).stdout


def adb_root_shell(cmd: str, *, check: bool = True) -> str:
    quoted = cmd.replace('"', '\\"')
    return adb_shell(f'su -c "{quoted}"', check=check)


def adb_root_cat(remote: str, local: Path) -> bool:
    local.parent.mkdir(parents=True, exist_ok=True)
    with local.open("wb") as handle:
        proc = subprocess.run(
            ["adb", "exec-out", "su", "-c", f"cat {remote}"],
            stdout=handle,
            stderr=subprocess.PIPE,
            text=False,
        )
    if proc.returncode != 0:
        local.unlink(missing_ok=True)
        return False
    os.chmod(local, 0o600)
    return True


def android_users() -> list[str]:
    out = adb_shell("pm list users")
    users = re.findall(r"UserInfo\{(\d+):", out)
    return users or ["0"]


def find_app_dirs() -> list[tuple[str, str]]:
    found: list[tuple[str, str]] = []
    for user in android_users():
        path = f"/data/user/{user}/{PKG}"
        if adb_root_shell(f"test -d {path} && echo yes || true").strip() == "yes":
            found.append((user, path))
    return found


def realm_paths(app_dir: str) -> list[str]:
    out = adb_root_shell(f"find {app_dir}/files/oura -name assa-store.realm -type f 2>/dev/null || true")
    return [line.strip() for line in out.splitlines() if line.strip()]


def key_candidates_near_ring_configs(data: bytes, serial: bytes | None) -> list[bytes]:
    candidates: list[bytes] = []
    windows: list[tuple[int, int]] = []
    for marker in (b"auth_key", b"serial_number"):
        for m in re.finditer(re.escape(marker), data):
            windows.append((max(0, m.start() - 8192), min(len(data), m.end() + 8192)))
    if serial:
        for m in re.finditer(re.escape(serial), data):
            windows.append((max(0, m.start() - 8192), min(len(data), m.end() + 8192)))

    for start, end in windows:
        for m in HEX_KEY_RE.finditer(data[start:end]):
            value = m.group(0)
            if value not in candidates and not value.startswith(b"0" * 8):
                candidates.append(value)
    return candidates


def binary_candidates_near_ring_configs(data: bytes, serial: bytes | None) -> list[tuple[int, bytes]]:
    """Return high-entropy 16-byte candidates near Realm ring config markers.

    Realm stores `DbRingConfiguration.auth_key` as a binary/blob property in the
    current Android app. The raw file layout is not stable enough to identify the
    field by offset alone, so these candidates must be verified with the BLE
    nonce/authenticate flow before use.
    """
    markers = [b"auth_key", b"serial_number", b"android_ble_identifier", b"DbRingConfiguration"]
    if serial:
        markers.append(serial)

    windows: list[tuple[int, int]] = []
    for marker in markers:
        for match in re.finditer(re.escape(marker), data):
            windows.append((max(0, match.start() - 4096), min(len(data), match.end() + 4096)))

    candidates: list[tuple[int, bytes]] = []
    seen: set[bytes] = set()
    for start, end in windows:
        chunk = data[start:end]
        for offset in range(0, max(0, len(chunk) - 15)):
            value = chunk[offset : offset + 16]
            if value in seen or not looks_like_binary_key(value):
                continue
            seen.add(value)
            candidates.append((start + offset, value))
    return candidates


def summarize_log(log: str) -> list[str]:
    interesting = []
    for line in log.splitlines():
        if any(term in line for term in ("hasRingConfiguration", "hasPairedRing", "No paired Ring", "hasActiveRings", "ScanFinished", "PermissionNeeded")):
            interesting.append(line)
    return interesting[-20:]


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out-dir", default="captures/android", help="Directory for pulled Android artifacts")
    parser.add_argument("--serial", help="Expected ring serial, used to narrow key candidates")
    parser.add_argument("--key-out", default="captures/android/oura-android-auth.key", help="Key output path")
    parser.add_argument("--candidate-out", default="captures/android/oura-android-auth-candidates.txt", help="Binary candidate output path")
    args = parser.parse_args()

    try:
        adb_root_shell("id")
    except subprocess.CalledProcessError as exc:
        print("adb root via Magisk su failed; connect the rooted phone and allow the su prompt", file=sys.stderr)
        print(exc.stderr, file=sys.stderr)
        return 2

    app_dirs = find_app_dirs()
    if not app_dirs:
        print(f"{PKG} app data not found under /data/user/*")
        return 1

    out_dir = Path(args.out_dir)
    expected_serial = args.serial.encode() if args.serial else None
    wrote_key = False

    for user, app_dir in app_dirs:
        print(f"Android user {user}: {app_dir}")
        log_local = out_dir / f"user-{user}-app.log"
        if adb_root_cat(f"{app_dir}/files/app.log", log_local):
            log_text = log_local.read_text(errors="replace")
            for line in summarize_log(log_text):
                print(f"  log: {line}")

        for remote in realm_paths(app_dir):
            local = out_dir / f"user-{user}-{Path(remote).parent.name}-assa-store.realm"
            if not adb_root_cat(remote, local):
                print(f"  failed to pull {remote}")
                continue
            data = local.read_bytes()
            serials = sorted(set(m.group(0).decode() for m in SERIAL_RE.finditer(data)))
            if serials:
                print(f"  serials in Realm: {', '.join(serials[:8])}")
            serial = expected_serial or (serials[0].encode() if len(serials) == 1 else None)
            candidates = key_candidates_near_ring_configs(data, serial)
            print(f"  plausible auth_key candidates near ring config: {len(candidates)}")
            if len(candidates) == 1:
                key_out = Path(args.key_out)
                key_out.parent.mkdir(parents=True, exist_ok=True)
                key_out.write_bytes(candidates[0] + b"\n")
                os.chmod(key_out, 0o600)
                print(f"  wrote key to {key_out}")
                wrote_key = True
            binary_candidates = binary_candidates_near_ring_configs(data, serial)
            if binary_candidates:
                candidate_out = Path(args.candidate_out)
                candidate_out.parent.mkdir(parents=True, exist_ok=True)
                with candidate_out.open("w", encoding="utf-8") as handle:
                    for offset, value in binary_candidates:
                        handle.write(f"{offset}\t{value.hex()}\n")
                os.chmod(candidate_out, 0o600)
                print(f"  wrote {len(binary_candidates)} binary candidates to {candidate_out}")

    if not wrote_key:
        print("No single verified auth key found. Verify binary candidates with BLE app-auth before use.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
