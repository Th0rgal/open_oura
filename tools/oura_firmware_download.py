#!/usr/bin/env python3
"""Download Oura ring firmware/OTA packages the way the Android app does.

Reverse-engineered flow (see docs/firmware-update.md):

  1. POST https://api.ouraring.com/api/v2/client/config   (account bearer)
       body = ConfigParams{device, rings[], accessories[], format, ...}
       -> ClientConfiguration{ ota_files: [{type, version, slug}], firmware_updates: [...] }
  2. GET  https://api.ouraring.com/api/v2/file/{type}/{version}/{slug}   (account bearer)
       -> OtaPackageManifest{ filename, md5, sha256, uploaded_at, size, url }
  3. GET  <manifest.url>   (octet-stream)
       -> the raw CYACD2 / binary OTA image; verified against md5/sha256/size.

The bearer is the Oura *account* session token (NOT the ring BLE key). It is
minted by the app's attested Curity HAAPI login and cannot be produced by a
script -- capture it from a logged-in app session (e.g. mitmproxy) and pass it
here via --token or the OURA_BEARER env var.

This tool deliberately uses only the Python stdlib (no extra deps).

Examples
--------
  # You captured the token AND the type/version/slug (from mitmproxy's
  # /api/v2/client/config response, or a /api/v2/file/... request):
  OURA_BEARER='eyJ...' ./oura_firmware_download.py \
      --type firmware_oreo --version 3.4.3 --slug abc123 --out ./fw

  # You only have the token -- try to discover descriptors via /client/config.
  # If it returns nothing, paste the captured config JSON instead:
  OURA_BEARER='eyJ...' ./oura_firmware_download.py --config-json captured_config.json --out ./fw

  # You captured a full manifest JSON (has the absolute `url`): just fetch+verify.
  ./oura_firmware_download.py --manifest-json captured_manifest.json --out ./fw
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
import sys
import urllib.error
import urllib.request
from urllib.parse import urlparse

API_BASE = "https://api.ouraring.com"
API_HOST = "api.ouraring.com"


def _is_api_url(url: str) -> bool:
    """True only for https://api.ouraring.com (exact host) -- not lookalikes."""
    p = urlparse(url)
    return p.scheme == "https" and p.hostname == API_HOST


def _safe_filename(manifest: dict) -> str:
    """Basename-only output filename so a hostile manifest can't escape out_dir."""
    fname = os.path.basename(manifest.get("filename") or "")
    if fname in ("", ".", ".."):
        fname = f"{manifest.get('type','fw')}_{manifest.get('version','0')}.bin"
    return fname
# A plausible app UA; the real one comes from EndpointKt.getUserAgent(appConfig).
USER_AGENT = "okhttp/4.12.0"


def _req(url: str, *, token: str | None, method: str = "GET",
         body: bytes | None = None, accept: str = "application/json",
         content_type: str | None = None) -> tuple[int, dict, bytes]:
    headers = {"User-Agent": USER_AGENT, "Accept": accept}
    if token:
        headers["Authorization"] = token if token.lower().startswith("bearer ") else f"Bearer {token}"
    if content_type:
        headers["Content-Type"] = content_type
    r = urllib.request.Request(url, data=body, method=method, headers=headers)
    try:
        with urllib.request.urlopen(r, timeout=60) as resp:
            return resp.status, dict(resp.headers), resp.read()
    except urllib.error.HTTPError as e:
        return e.code, dict(e.headers), e.read()


def get_config(token: str, params: dict) -> dict:
    body = json.dumps(params).encode()
    status, _, data = _req(f"{API_BASE}/api/v2/client/config", token=token,
                           method="POST", body=body,
                           content_type="application/json")
    if status != 200:
        raise SystemExit(f"/client/config -> HTTP {status}: {data[:500].decode('utf-8','replace')}")
    return json.loads(data)


def get_manifest(token: str, type_: str, version: str, slug: str) -> dict:
    url = f"{API_BASE}/api/v2/file/{type_}/{version}/{slug}"
    status, _, data = _req(url, token=token)
    if status != 200:
        raise SystemExit(f"manifest {url} -> HTTP {status}: {data[:500].decode('utf-8','replace')}")
    return json.loads(data)


def download_image(manifest: dict, out_dir: str, token: str | None) -> str:
    url = manifest["url"]
    # The manifest URL is usually a presigned CDN link (no auth). Send the
    # bearer only if it's back on api.ouraring.com.
    use_token = token if _is_api_url(url) else None
    status, _, data = _req(url, token=use_token, accept="application/octet-stream")
    if status != 200:
        raise SystemExit(f"download {url} -> HTTP {status}")

    os.makedirs(out_dir, exist_ok=True)
    path = os.path.join(out_dir, _safe_filename(manifest))

    # Integrity checks against the manifest.
    problems = []
    if "size" in manifest and manifest["size"] and len(data) != manifest["size"]:
        problems.append(f"size {len(data)} != manifest {manifest['size']}")
    if manifest.get("md5"):
        got = hashlib.md5(data).hexdigest()
        if got.lower() != manifest["md5"].lower():
            problems.append(f"md5 {got} != manifest {manifest['md5']}")
    if manifest.get("sha256"):
        got = hashlib.sha256(data).hexdigest()
        if got.lower() != manifest["sha256"].lower():
            problems.append(f"sha256 {got} != manifest {manifest['sha256']}")

    with open(path, "wb") as f:
        f.write(data)
    print(f"  saved {path}  ({len(data)} bytes)")
    print(f"    md5    {hashlib.md5(data).hexdigest()}")
    print(f"    sha256 {hashlib.sha256(data).hexdigest()}")
    if problems:
        print("  !! INTEGRITY MISMATCH: " + "; ".join(problems), file=sys.stderr)
    else:
        print("  integrity OK")
    return path


def build_config_params(args) -> dict:
    """Best-effort minimal ConfigParams. If the server returns no ota_files,
    capture the real body/response from mitmproxy instead."""
    rings = []
    if args.ring_serial or args.ring_fw or args.ring_hw:
        rings.append({
            "mac_address": args.ring_mac or "",
            "bootloader_version": args.ring_bootloader or "",
            "firmware_version": args.ring_fw or "",
            "hardware_type": args.ring_hw or "",
            "hardware_version": args.ring_hwver or "",
            "serial_number": args.ring_serial or "",
            "color": "",
            "design": "",
            "capabilities": [],
            "last_synced": "",
        })
    return {
        "device": {"device_uid": args.device_uid, "model": "iPhone",
                   "os_version": "17.0", "os": "ios"},
        "rings": rings,
        "accessories": [],
        "format": args.format,
        "user": {"country_of_residence": "US"},
    }


def main() -> None:
    ap = argparse.ArgumentParser(description="Download Oura firmware/OTA packages.")
    ap.add_argument("--token", default=os.environ.get("OURA_BEARER"),
                    help="Account bearer (or set OURA_BEARER). 'Bearer ' prefix optional.")
    ap.add_argument("--out", default="./firmware", help="Output directory")
    # Direct descriptor (skip /client/config):
    ap.add_argument("--type", help="package type, e.g. firmware_oreo")
    ap.add_argument("--version")
    ap.add_argument("--slug")
    # Captured JSON shortcuts:
    ap.add_argument("--config-json", help="Path to a captured /client/config response JSON")
    ap.add_argument("--manifest-json", help="Path to a captured manifest JSON (has absolute url)")
    ap.add_argument("--list", action="store_true", help="Only list discovered packages")
    # Optional ConfigParams hints for discovery:
    ap.add_argument("--device-uid", default="00000000-0000-0000-0000-000000000000")
    ap.add_argument("--format", type=int, default=1)
    ap.add_argument("--ring-mac"); ap.add_argument("--ring-serial")
    ap.add_argument("--ring-fw"); ap.add_argument("--ring-bootloader")
    ap.add_argument("--ring-hw"); ap.add_argument("--ring-hwver")
    args = ap.parse_args()

    # Path 0: a captured manifest -> just download+verify (token optional).
    if args.manifest_json:
        manifest = json.load(open(args.manifest_json))
        download_image(manifest, args.out, args.token)
        return

    # Gather descriptors [(type, version, slug)] either directly, from a
    # captured config response, or by calling /client/config.
    descriptors = []
    if args.type and args.version and args.slug:
        descriptors = [(args.type, args.version, args.slug)]
    elif args.config_json:
        cfg = json.load(open(args.config_json))
        descriptors = [(d["type"], d["version"], d["slug"]) for d in cfg.get("ota_files", [])]
    else:
        if not args.token:
            raise SystemExit("Need --token/OURA_BEARER (or --config-json / --manifest-json).")
        cfg = get_config(args.token, build_config_params(args))
        descriptors = [(d["type"], d["version"], d["slug"]) for d in cfg.get("ota_files", [])]
        print("firmware_updates:", json.dumps(cfg.get("firmware_updates", []), indent=2))

    if not descriptors:
        raise SystemExit("No ota_files discovered. Capture the /api/v2/client/config "
                         "response in mitmproxy and pass it via --config-json, or pass "
                         "--type/--version/--slug from a captured /api/v2/file/... request.")

    print(f"discovered {len(descriptors)} package(s):")
    for t, v, s in descriptors:
        print(f"  {t}  v{v}  slug={s}")
    if args.list:
        return
    if not args.token:
        raise SystemExit("Need --token/OURA_BEARER to fetch manifests + images.")

    for t, v, s in descriptors:
        print(f"\n== {t} v{v} ==")
        manifest = get_manifest(args.token, t, v, s)
        print("  manifest:", json.dumps({k: manifest.get(k) for k in
              ("filename", "size", "md5", "sha256", "uploaded_at")}, indent=2))
        download_image(manifest, args.out, args.token)


if __name__ == "__main__":
    main()
