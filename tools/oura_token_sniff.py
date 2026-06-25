"""mitmproxy addon: surface the Oura cloud session/auth token.

Run:  mitmweb --listen-port 8080 -s tools/oura_token_sniff.py
(or)  mitmdump --listen-port 8080 -s tools/oura_token_sniff.py

It watches every request/response whose host contains "oura" and prints any
bearer token, cookie, or token-looking JSON field it sees, so you can grab the
session token without scrolling the flow list. Captured tokens are also appended
to tools/oura_tokens.txt (gitignored).
"""

import json
import os
import re
from pathlib import Path

from mitmproxy import ctx, http

OUT = Path(__file__).with_name("oura_tokens.txt")
_seen: set[str] = set()

# JSON keys that commonly hold a token in login/refresh responses.
TOKEN_KEYS = re.compile(r"(access[_-]?token|id[_-]?token|refresh[_-]?token|session|auth[_-]?token|jwt|bearer)", re.I)


def _emit(label: str, value: str, host: str) -> None:
    key = f"{label}:{value}"
    if key in _seen:
        return
    _seen.add(key)
    ctx.log.alert(f"[OURA TOKEN] {host} {label}: {value}")
    new = not OUT.exists()
    with OUT.open("a") as f:
        f.write(f"{host}\t{label}\t{value}\n")
    if new:
        os.chmod(OUT, 0o600)  # file holds full account tokens


def _scan_json(obj, host: str, path: str = "") -> None:
    if isinstance(obj, dict):
        for k, v in obj.items():
            if isinstance(v, str) and TOKEN_KEYS.search(str(k)) and len(v) > 12:
                _emit(f"json {path}{k}", v, host)
            _scan_json(v, host, f"{path}{k}.")
    elif isinstance(obj, list):
        for i, v in enumerate(obj):
            _scan_json(v, host, f"{path}{i}.")


def request(flow: http.HTTPFlow) -> None:
    host = flow.request.pretty_host
    if "oura" not in host:
        return
    auth = flow.request.headers.get("Authorization")
    if auth:
        _emit("Authorization header", auth, host)
    cookie = flow.request.headers.get("Cookie")
    if cookie:
        _emit("Cookie header", cookie, host)


def response(flow: http.HTTPFlow) -> None:
    host = flow.request.pretty_host
    if "oura" not in host:
        return
    ct = flow.response.headers.get("Content-Type", "")
    if "json" in ct and flow.response.content:
        try:
            _scan_json(json.loads(flow.response.get_text()), host)
        except (ValueError, json.JSONDecodeError):
            pass
    for sc in flow.response.headers.get_all("Set-Cookie"):
        if TOKEN_KEYS.search(sc):
            _emit("Set-Cookie", sc, host)
