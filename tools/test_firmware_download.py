"""Security regression checks for the firmware downloader (Codex findings 6 & 7).

Run: python3 tools/test_firmware_download.py
"""
import os

from oura_firmware_download import _is_api_url, _safe_filename


def test_host_check():
    # Real host gets the token.
    assert _is_api_url("https://api.ouraring.com/api/v2/file/x")
    # Lookalikes / downgrades do NOT (would leak the bearer otherwise).
    assert not _is_api_url("https://api.ouraring.com.evil.example/fw.bin")
    assert not _is_api_url("http://api.ouraring.com/fw.bin")          # not https
    assert not _is_api_url("https://evil.example/api.ouraring.com")    # host in path
    assert not _is_api_url("https://api-ouraring.com/fw.bin")


def test_filename_containment():
    out = "/safe/out"
    for hostile in ("../../outside.txt", "/tmp/absolute.bin", "..", ".", "", "a/b/c.bin"):
        path = os.path.join(out, _safe_filename({"filename": hostile}))
        # Resolved path must stay directly under out.
        assert os.path.dirname(os.path.normpath(path)) == out, (hostile, path)
    # A normal filename is preserved.
    assert _safe_filename({"filename": "fw.bin"}) == "fw.bin"
    # Missing filename falls back to a synthesized basename.
    assert _safe_filename({"type": "firmware_oreo", "version": "3.4.3"}) == "firmware_oreo_3.4.3.bin"


if __name__ == "__main__":
    test_host_check()
    test_filename_containment()
    print("ok")
