#!/usr/bin/env python3
"""Run Oura BLE protocol probes and capture raw request/response packets."""

import argparse
import asyncio
import json
import os
import secrets
import struct
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path
from typing import Optional

from bleak import BleakClient, BleakScanner
from bleak.exc import BleakError
from Crypto.Cipher import AES
from Crypto.Util.Padding import pad


OURA_SERVICE = "98ed0001-a541-11e4-b6a0-0002a5d5c51b"
OURA_NOTIFY = "98ed0003-a541-11e4-b6a0-0002a5d5c51b"
OURA_WRITE = "98ed0002-a541-11e4-b6a0-0002a5d5c51b"

AUTH_RESULTS = {
    0x00: "success",
    0x01: "authentication_error",
    0x02: "in_factory_reset",
    0x03: "not_original_onboarded_device",
}

DFU_STATUS = {
    0x00: "success",
    0x01: "incomplete_image",
    0x02: "image_validation_failed",
    0x03: "downgrade_not_allowed",
    0x04: "other_error",
    0x05: "battery_low",
    0x06: "sleep",
    0x07: "disabled",
    0x08: "rdata",
    0x09: "sync_time",
}

FEATURE_SET_RESULTS = {
    0x00: "success",
    0x01: "not_supported",
    0x02: "not_available",
    0x03: "not_in_finger",
    0x04: "message_too_short",
    0x05: "low_battery",
}

FEATURE_MODES = {
    0x00: "off",
    0x01: "automatic",
    0x02: "requested",
    0x03: "requested_subscription",
}

FEATURE_STATES = {
    0x00: "idle",
    0x01: "scanning",
    0x02: "measuring",
    0x03: "postprocessing",
}

FEATURE_SUBSCRIPTIONS = {
    0x00: "off",
    0x01: "state",
    0x02: "latest",
}

START_FW_RESULTS = {
    0x00: "success",
    0x01: "battery_level_too_low",
    0x02: "sleep_analysis_in_progress",
}

EVENT_TAGS = {
    0x41: "ring_start",
    0x42: "time_sync",
    0x43: "debug_event",
    0x44: "ibi_event",
    0x45: "state_change",
    0x46: "temp_event",
    0x47: "motion_event",
    0x48: "sleep_period_information",
    0x49: "sleep_summary_1",
    0x4A: "ppg_amplitude",
    0x4B: "sleep_phase_information",
    0x4C: "sleep_summary_2",
    0x4D: "ring_sleep_feature_information",
    0x4E: "sleep_phase_details",
    0x4F: "sleep_summary_3",
    0x50: "activity_information",
    0x51: "activity_summary_1",
    0x52: "activity_summary_2",
    0x53: "wear_event",
    0x54: "recovery_summary",
    0x55: "sleep_heart_rate",
    0x56: "alert_event",
    0x57: "ring_sleep_feature_information_2",
    0x58: "sleep_summary_4",
    0x59: "eda_event",
    0x5A: "sleep_phase_data",
    0x5B: "ble_connection",
    0x5C: "user_information",
    0x5D: "hrv_event",
    0x5E: "self_test_event",
    0x5F: "raw_acm_event",
    0x60: "ibi_and_amplitude_event",
    0x61: "debug_data",
    0x62: "on_demand_meas",
    0x63: "ppg_peak_event",
    0x64: "raw_ppg_event",
    0x65: "on_demand_session",
    0x66: "on_demand_motion",
    0x67: "raw_ppg_summary",
    0x68: "raw_ppg_data",
    0x69: "temp_period",
    0x6A: "sleep_period_information_2",
    0x6B: "motion_period",
    0x6C: "feature_session",
    0x6D: "meas_quality_event",
    0x6E: "spo2_ibi_and_amplitude_event",
    0x6F: "spo2_event",
    0x70: "spo2_smoothed_event",
    0x71: "green_ibi_and_amplitude_event",
    0x72: "sleep_acm_period",
    0x73: "ehr_trace_event",
    0x74: "ehr_acm_intensity_event",
    0x75: "sleep_temp_event",
    0x76: "bedtime_period",
    0x77: "spo2_dc_event",
    0x79: "self_test_data_event",
    0x7A: "tag_event",
    0x7E: "real_step_event_feature_1",
    0x7F: "real_step_event_feature_2",
    0x81: "cva_raw_ppg_data",
    0x82: "scan_start",
    0x83: "scan_end",
}


@dataclass(frozen=True)
class Command:
    name: str
    request: bytes
    safety: str
    notes: str


def packet(tag: int, payload: bytes = b"") -> bytes:
    if len(payload) > 255:
        raise ValueError("payload too long for one Oura packet")
    return bytes([tag, len(payload)]) + payload


def get_events(start_timestamp: int = 0, max_events: int = 8, flags: int = -1) -> bytes:
    return packet(0x10, struct.pack("<IBi", start_timestamp, max_events, flags))


def sync_time(ts: Optional[int] = None, timezone_half_hours: int = 0) -> bytes:
    timestamp = int(datetime.now(timezone.utc).timestamp()) if ts is None else ts
    return packet(0x12, struct.pack("<QB", timestamp, timezone_half_hours & 0xFF))


COMMANDS: dict[str, Command] = {
    "unknown_01": Command("unknown_01", packet(0x01), "unknown", "Ringverse: no response observed."),
    "unknown_02": Command("unknown_02", packet(0x02), "unknown", "Ringverse: no response observed."),
    "rdata_state": Command("rdata_state", bytes.fromhex("030105"), "safe", "RData collection state, no datatype filter."),
    "rdata_state_none": Command("rdata_state_none", bytes.fromhex("03020500"), "safe", "RData collection state for datatype NONE."),
    "rdata_get_page0": Command("rdata_get_page0", bytes.fromhex("0303010000"), "data", "RData page 0 request."),
    "rdata_start_none_zero": Command("rdata_start_none_zero", bytes.fromhex("030a02000000000000000000"), "state", "Configure/start RData with datatype NONE and zero timestamps."),
    "rdata_stop": Command("rdata_stop", bytes.fromhex("030103"), "state", "Stop RData recording if active."),
    "rdata_clear": Command("rdata_clear", bytes.fromhex("030104"), "danger", "Clear RData state/data."),
    "realtime_off": Command("realtime_off", bytes.fromhex("060400000000"), "state", "Disable realtime measurements."),
    "firmware": Command("firmware", bytes.fromhex("0803000000"), "safe", "Get firmware/API/BT stack/MAC."),
    "battery": Command("battery", packet(0x0C), "safe", "Get battery level."),
    "product_hardware_frodo": Command("product_hardware_frodo", bytes.fromhex("1803140010"), "safe", "Product info request 0."),
    "product_hardware": Command("product_hardware", bytes.fromhex("1803180010"), "safe", "Product info request 1."),
    "product_code": Command("product_code", bytes.fromhex("1803280009"), "safe", "Product info request 2."),
    "product_code_frodo": Command("product_code_frodo", bytes.fromhex("1803340004"), "safe", "Product info request 3."),
    "serial_old": Command("serial_old", bytes.fromhex("1803040010"), "safe", "Product info request 4."),
    "serial": Command("serial", bytes.fromhex("1803080010"), "safe", "Product info request 5."),
    "capabilities_page0": Command("capabilities_page0", bytes.fromhex("2f020100"), "safe", "Extended get capabilities page 0."),
    "capabilities_page1": Command("capabilities_page1", bytes.fromhex("2f020101"), "safe", "Extended get capabilities page 1."),
    "auth_nonce": Command("auth_nonce", bytes.fromhex("2f012b"), "safe", "Get app-auth nonce."),
    "feature_background_dfu": Command("feature_background_dfu", bytes.fromhex("2f022000"), "safe", "Get feature status 0x00."),
    "feature_research_data": Command("feature_research_data", bytes.fromhex("2f022001"), "safe", "Get feature status 0x01."),
    "feature_daytime_hr": Command("feature_daytime_hr", bytes.fromhex("2f022002"), "safe", "Get feature status 0x02."),
    "feature_exercise_hr": Command("feature_exercise_hr", bytes.fromhex("2f022003"), "safe", "Get feature status 0x03."),
    "feature_spo2": Command("feature_spo2", bytes.fromhex("2f022004"), "safe", "Get feature status 0x04."),
    "feature_bundling": Command("feature_bundling", bytes.fromhex("2f022005"), "safe", "Get feature status 0x05."),
    "feature_encrypted_api": Command("feature_encrypted_api", bytes.fromhex("2f022006"), "safe", "Get feature status 0x06."),
    "feature_tap_to_tag": Command("feature_tap_to_tag", bytes.fromhex("2f022007"), "safe", "Get feature status 0x07."),
    "feature_resting_hr": Command("feature_resting_hr", bytes.fromhex("2f022008"), "safe", "Get feature status 0x08."),
    "feature_app_auth": Command("feature_app_auth", bytes.fromhex("2f022009"), "safe", "Get feature status 0x09."),
    "feature_ble_mode": Command("feature_ble_mode", bytes.fromhex("2f02200a"), "safe", "Get feature status 0x0a."),
    "feature_real_steps": Command("feature_real_steps", bytes.fromhex("2f02200b"), "safe", "Get feature status 0x0b."),
    "feature_experimental": Command("feature_experimental", bytes.fromhex("2f02200c"), "safe", "Get feature status 0x0c."),
    "feature_cva_ppg_sampler": Command("feature_cva_ppg_sampler", bytes.fromhex("2f02200d"), "safe", "Get feature status 0x0d."),
    "events": Command("events", get_events(), "data", "Get up to 8 history events from timestamp 0."),
    "sync_time": Command("sync_time", b"", "state", "Set ring time to host UTC timestamp."),
    "check_sleep_analysis": Command("check_sleep_analysis", bytes.fromhex("280100"), "state", "Check sleep analysis without force."),
    "check_sleep_analysis_force": Command("check_sleep_analysis_force", bytes.fromhex("280101"), "state", "Check sleep analysis with force flag."),
    "set_notification_none": Command("set_notification_none", bytes.fromhex("1c0100"), "state", "Disable notification flags."),
    "set_notification_all": Command("set_notification_all", bytes.fromhex("1c013f"), "state", "Enable notification flags."),
    "set_user_gender_empty": Command("set_user_gender_empty", bytes.fromhex("2003020000"), "state", "Set unused gender field to empty/0."),
    "set_user_height_empty": Command("set_user_height_empty", bytes.fromhex("200403000000"), "state", "Set unused height field to empty/0."),
    "set_user_weight_empty": Command("set_user_weight_empty", bytes.fromhex("200404000000"), "state", "Set unused weight field to empty/0."),
    "set_user_unit_empty": Command("set_user_unit_empty", bytes.fromhex("2003060000"), "state", "Set unused unit-system field to empty/0."),
    "set_user_dob_zero": Command("set_user_dob_zero", bytes.fromhex("200a05000000000000000000"), "state", "Set date-of-birth field to zero."),
    "set_ble_mode_normal": Command("set_ble_mode_normal", bytes.fromhex("160100"), "state", "Set BLE mode to normal."),
    "set_ble_mode_fast_hr": Command("set_ble_mode_fast_hr", bytes.fromhex("160101"), "state", "Set BLE mode to fast HR."),
    "set_ble_mode_deep_sleep": Command("set_ble_mode_deep_sleep", bytes.fromhex("160102"), "danger", "Set BLE mode to deep sleep."),
    "set_ring_mode_normal": Command("set_ring_mode_normal", bytes.fromhex("310400000000"), "state", "Set ring mode normal using 4-byte Android shape."),
    "set_ring_mode_fast_hr": Command("set_ring_mode_fast_hr", bytes.fromhex("310401000000"), "state", "Set ring mode fast HR using 4-byte Android shape."),
    "set_ring_mode_deep_sleep": Command("set_ring_mode_deep_sleep", bytes.fromhex("310402000000"), "danger", "Set ring mode deep sleep using 4-byte Android shape."),
    "sync_manufacturing_info": Command("sync_manufacturing_info", bytes.fromhex("3900"), "safe", "Sync manufacturing info."),
    "feature_latest_daytime_hr": Command("feature_latest_daytime_hr", bytes.fromhex("2f022402"), "safe", "Get latest values for daytime HR."),
    "feature_latest_exercise_hr": Command("feature_latest_exercise_hr", bytes.fromhex("2f022403"), "safe", "Get latest values for exercise HR."),
    "feature_latest_spo2": Command("feature_latest_spo2", bytes.fromhex("2f022404"), "safe", "Get latest values for SPO2."),
    "feature_latest_charging": Command("feature_latest_charging", bytes.fromhex("2f02240e"), "safe", "Get latest values for charging control."),
    "feature_param_daytime_hr_0": Command("feature_param_daytime_hr_0", bytes.fromhex("2f03460200"), "safe", "Get feature param 0 for daytime HR."),
    "set_bundling_off": Command("set_bundling_off", bytes.fromhex("2f020300"), "state", "Set bundling disabled."),
    "set_bundling_on": Command("set_bundling_on", bytes.fromhex("2f020301"), "state", "Set bundling enabled."),
    "set_feature_daytime_hr_off": Command("set_feature_daytime_hr_off", bytes.fromhex("2f03220200"), "state", "Set daytime HR feature mode off."),
    "set_feature_daytime_hr_auto": Command("set_feature_daytime_hr_auto", bytes.fromhex("2f03220201"), "state", "Set daytime HR feature mode automatic."),
    "set_feature_daytime_hr_sub_off": Command("set_feature_daytime_hr_sub_off", bytes.fromhex("2f03260200"), "state", "Set daytime HR subscription off."),
    "set_feature_daytime_hr_sub_latest": Command("set_feature_daytime_hr_sub_latest", bytes.fromhex("2f03260202"), "state", "Set daytime HR subscription latest."),
    "set_feature_params_daytime_hr_0": Command("set_feature_params_daytime_hr_0", bytes.fromhex("2f03290200"), "state", "Set daytime HR feature parameter payload 00."),
    "set_manufacturing_test": Command("set_manufacturing_test", bytes.fromhex("37050103000000"), "danger", "Set manufacturing mode TEST."),
    "set_manufacturing_prod": Command("set_manufacturing_prod", bytes.fromhex("37050107000000"), "danger", "Set manufacturing mode PROD."),
    "run_self_test": Command("run_self_test", bytes.fromhex("0a04ffffffff"), "state", "Runs ring self-test."),
    "start_fw_update": Command("start_fw_update", bytes.fromhex("0e0100"), "danger", "Starts firmware update mode."),
    "factory_reset": Command("factory_reset", bytes.fromhex("1a00"), "danger", "Factory reset."),
    "enable_flight_mode": Command("enable_flight_mode", bytes.fromhex("26026027"), "danger", "Enables flight mode."),
    "dfu_reset": Command("dfu_reset", bytes.fromhex("2b0101"), "danger", "DFU reset."),
    "dfu_start_zero": Command("dfu_start_zero", bytes.fromhex("2b12020200000000000000000000000000000000"), "danger", "DFU start with zero image metadata."),
    "dfu_block_zero": Command("dfu_block_zero", bytes.fromhex("2b0c030200000000000000000000"), "danger", "DFU block transfer header with zero metadata."),
    "dfu_activate_zero": Command("dfu_activate_zero", bytes.fromhex("2b06040200000000"), "danger", "DFU activate with zero CRC."),
}

SAFE_MATRIX = [
    "firmware",
    "battery",
    "auth_nonce",
    "capabilities_page0",
    "capabilities_page1",
    "product_hardware_frodo",
    "product_hardware",
    "product_code",
    "product_code_frodo",
    "serial_old",
    "serial",
    "feature_background_dfu",
    "feature_research_data",
    "feature_daytime_hr",
    "feature_exercise_hr",
    "feature_spo2",
    "feature_bundling",
    "feature_encrypted_api",
    "feature_tap_to_tag",
    "feature_resting_hr",
    "feature_app_auth",
    "feature_ble_mode",
    "feature_real_steps",
    "feature_experimental",
    "feature_cva_ppg_sampler",
]

AUTH_SAFE_MATRIX = [
    "auth",
    "battery",
    "feature_background_dfu",
    "feature_research_data",
    "feature_daytime_hr",
    "feature_exercise_hr",
    "feature_spo2",
    "feature_bundling",
    "feature_encrypted_api",
    "feature_tap_to_tag",
    "feature_resting_hr",
    "feature_app_auth",
    "feature_ble_mode",
    "feature_real_steps",
    "feature_experimental",
    "feature_cva_ppg_sampler",
    "rdata_state",
    "rdata_state_none",
    "sync_manufacturing_info",
    "feature_latest_daytime_hr",
    "feature_latest_exercise_hr",
    "feature_latest_spo2",
    "feature_latest_charging",
    "feature_param_daytime_hr_0",
]


def parse_packet(data: bytes) -> dict:
    parsed: dict[str, object] = {"hex": data.hex()}
    if len(data) < 2:
        parsed["error"] = "short_packet"
        return parsed
    tag = data[0]
    length = data[1]
    payload = data[2:]
    parsed.update(
        {
            "tag": tag,
            "length": length,
            "payload_hex": payload.hex(),
            "length_ok": length == len(payload),
        }
    )
    if tag == 0x09 and len(payload) >= 18:
        parsed.update(
            {
                "api_version": ".".join(str(x) for x in payload[0:3]),
                "firmware_version": ".".join(str(x) for x in payload[3:6]),
                "bootloader_version": ".".join(str(x) for x in payload[6:9]),
                "bt_stack_version": ".".join(str(x) for x in payload[9:12]),
                "mac": ":".join(f"{byte:02x}" for byte in reversed(payload[12:18])),
            }
        )
    elif tag == 0x03 and len(payload) >= 2:
        parsed.update(
            {
                "rdata_subtag": payload[0],
                "rdata_status": payload[1],
                "rdata_data_hex": payload[2:].hex(),
            }
        )
    elif tag == 0x07 and payload:
        parsed["realtime_status"] = payload[0]
    elif tag == 0x0B and payload:
        parsed["self_test_payload_hex"] = payload.hex()
    elif tag == 0x0D and len(payload) >= 6:
        parsed.update(
            {
                "battery_percent": payload[0],
                "charging_progress": payload[1],
                "charging_recommended": payload[2],
                "battery_unknown_tail": payload[3:].hex(),
            }
        )
    elif tag == 0x0F and payload:
        parsed["start_fw_update_status"] = payload[0]
        parsed["start_fw_update_status_name"] = START_FW_RESULTS.get(payload[0], "unknown")
    elif tag == 0x19 and payload:
        parsed["product_status"] = payload[0]
        parsed["product_data_hex"] = payload[1:].hex()
        if payload[0] == 0 and len(payload) > 1:
            parsed["product_data_ascii"] = payload[1:].rstrip(b"\x00").decode("utf-8", "replace")
    elif tag == 0x1B and len(payload) >= 2:
        parsed["factory_reset_status"] = struct.unpack("<H", payload[:2])[0]
    elif tag == 0x1D and payload:
        parsed["notification_status"] = payload[0]
    elif tag == 0x2F and payload:
        ext = payload[0]
        parsed["extended_tag"] = ext
        if ext == 0x02 and len(payload) >= 2:
            parsed["capabilities_pages"] = payload[1]
            parsed["capability_pairs"] = [
                {"feature": payload[i], "value": payload[i + 1]}
                for i in range(2, len(payload) - 1, 2)
            ]
        elif ext == 0x21 and len(payload) >= 6:
            parsed.update(
                {
                    "feature": payload[1],
                    "feature_mode": payload[2],
                    "feature_status": payload[3],
                    "feature_state": payload[4],
                    "feature_subscription": payload[5],
                    "feature_mode_name": FEATURE_MODES.get(payload[2], "unknown"),
                    "feature_state_name": FEATURE_STATES.get(payload[4], "unknown"),
                    "feature_subscription_name": FEATURE_SUBSCRIPTIONS.get(payload[5], "unknown"),
                }
            )
        elif ext == 0x04 and len(payload) >= 2:
            parsed["bundling_enabled"] = payload[1]
        elif ext == 0x23 and len(payload) >= 3:
            parsed.update(
                {
                    "feature_set_mode": payload[1],
                    "feature_set_mode_result": payload[2],
                    "feature_set_mode_result_name": FEATURE_SET_RESULTS.get(payload[2], "unknown"),
                }
            )
        elif ext == 0x25 and len(payload) >= 7:
            parsed.update(
                {
                    "feature_latest": payload[1],
                    "feature_latest_result": payload[2],
                    "feature_latest_status": payload[3],
                    "feature_latest_state": payload[4],
                    "feature_latest_counter": struct.unpack("<H", payload[5:7])[0],
                    "feature_latest_data_hex": payload[7:].hex(),
                    "feature_latest_result_name": FEATURE_SET_RESULTS.get(payload[2], "unknown"),
                }
            )
        elif ext == 0x27 and len(payload) >= 3:
            parsed.update(
                {
                    "feature_subscription_feature": payload[1],
                    "feature_subscription_result": payload[2],
                    "feature_subscription_result_name": FEATURE_SET_RESULTS.get(payload[2], "unknown"),
                }
            )
        elif ext == 0x2A and len(payload) >= 3:
            parsed.update(
                {
                    "feature_params_feature": payload[1],
                    "feature_params_status": payload[2],
                    "feature_params_status_name": FEATURE_SET_RESULTS.get(payload[2], "unknown"),
                }
            )
        elif ext == 0x47 and len(payload) >= 4:
            parsed.update(
                {
                    "feature_param_feature": payload[1],
                    "feature_param_id": payload[2],
                    "feature_param_result": payload[3],
                    "feature_param_data_hex": payload[4:].hex(),
                }
            )
        elif ext == 0x2C:
            parsed["auth_nonce"] = payload[1:].hex()
        elif ext == 0x2E and len(payload) >= 2:
            parsed["auth_state"] = payload[1]
            parsed["auth_state_name"] = AUTH_RESULTS.get(payload[1], "unknown")
        elif ext == 0x2F and len(payload) >= 2:
            parsed["auth_state"] = payload[1]
            parsed["auth_state_name"] = AUTH_RESULTS.get(payload[1], "unknown")
    elif tag == 0x2B and payload:
        parsed["dfu_mode"] = payload[0]
        if len(payload) >= 2:
            parsed["dfu_status"] = payload[1]
            parsed["dfu_status_name"] = DFU_STATUS.get(payload[1], "unknown")
    elif tag == 0x25 and payload:
        parsed["set_auth_key_status"] = payload[0]
    elif tag == 0x11 and payload:
        parsed["events_received"] = payload[0]
        if len(payload) >= 2:
            parsed["sleep_analysis_progress"] = payload[1]
        if len(payload) >= 6:
            parsed["bytes_left"] = struct.unpack("<I", payload[2:6])[0]
    elif tag >= 0x41:
        parsed["event_tag"] = tag
        parsed["event_name"] = EVENT_TAGS.get(tag, "unknown")
        if len(payload) >= 4:
            parsed["event_timestamp"] = struct.unpack("<I", payload[:4])[0]
            parsed["event_payload_hex"] = payload[4:].hex()
            try:
                parsed["event_payload_ascii"] = payload[4:].rstrip(b"\x00").decode("utf-8")
            except UnicodeDecodeError:
                pass
    elif tag == 0x21 and len(payload) >= 2:
        parsed["user_info_type"] = payload[0]
        parsed["user_info_result"] = payload[1]
    elif tag == 0x29 and payload:
        parsed["sleep_analysis_status"] = payload[0]
    elif tag == 0x32 and len(payload) >= 4:
        parsed["ring_mode_status"] = struct.unpack("<I", payload[:4])[0] & 0xFFFFFF
    elif tag == 0x38 and payload:
        parsed["set_manufacturing_status"] = payload[0]
    elif tag == 0x3A and payload:
        parsed["sync_manufacturing_status"] = payload[0]
    return parsed


def describe_packet(data: bytes) -> str:
    parsed = parse_packet(data)
    fields = [f"hex={parsed['hex']}"]
    if "tag" in parsed:
        fields.append(f"tag=0x{parsed['tag']:02x}")
        fields.append(f"len={parsed['length']}")
    for key in (
        "api_version",
        "firmware_version",
        "bt_stack_version",
        "mac",
        "battery_percent",
        "auth_nonce",
        "auth_state",
        "auth_state_name",
        "set_auth_key_status",
        "product_data_ascii",
        "feature",
        "feature_mode",
        "feature_mode_name",
        "feature_status",
        "feature_state",
        "feature_state_name",
        "feature_subscription",
        "feature_subscription_name",
        "feature_latest",
        "feature_latest_result",
        "feature_latest_result_name",
        "feature_param_feature",
        "feature_param_id",
        "feature_param_result",
        "feature_set_mode",
        "feature_set_mode_result_name",
        "feature_subscription_result_name",
        "feature_params_status_name",
        "bundling_enabled",
        "rdata_subtag",
        "rdata_status",
        "realtime_status",
        "notification_status",
        "sleep_analysis_status",
        "user_info_type",
        "user_info_result",
        "ring_mode_status",
        "sync_manufacturing_status",
        "start_fw_update_status_name",
        "factory_reset_status",
        "dfu_status_name",
        "event_name",
        "events_received",
    ):
        if key in parsed:
            fields.append(f"{key}={parsed[key]}")
    return " ".join(fields)


def write_capture(path: Optional[Path], record: dict) -> None:
    if not path:
        return
    path.parent.mkdir(parents=True, exist_ok=True)
    new = not path.exists()
    with path.open("a", encoding="utf-8") as handle:
        handle.write(json.dumps(record, sort_keys=True) + "\n")
    if new:
        os.chmod(path, 0o600)  # captures hold serials/MAC-derived ids


def load_key(args: argparse.Namespace) -> Optional[bytes]:
    if args.auth_key:
        key = bytes.fromhex(args.auth_key)
    elif args.auth_key_file and Path(args.auth_key_file).exists():
        key = bytes.fromhex(Path(args.auth_key_file).read_text(encoding="utf-8").strip())
    else:
        return None
    if len(key) != 16:
        raise ValueError("auth key must be 16 bytes")
    return key


def save_key(path: Optional[str], key: bytes) -> None:
    if not path:
        return
    target = Path(path)
    target.parent.mkdir(parents=True, exist_ok=True)
    target.write_text(key.hex() + "\n", encoding="utf-8")
    os.chmod(target, 0o600)


async def find_device(args: argparse.Namespace):
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
    candidates.sort(key=lambda item: item[1].rssi, reverse=True)
    return candidates


async def transact(client: BleakClient, request: bytes, listen_seconds: float, capture: Optional[Path], name: str) -> list[bytes]:
    responses: list[bytes] = []

    def on_notify(sender, data: bytearray) -> None:
        payload = bytes(data)
        responses.append(payload)
        record = {
            "utc": datetime.now(timezone.utc).isoformat(),
            "command": name,
            "direction": "notification",
            "sender": str(sender),
            "packet": parse_packet(payload),
        }
        write_capture(capture, record)
        print(f"notification command={name} sender={sender} {describe_packet(payload)}")

    # set_auth_key writes are packet 0x24 || <16-byte ring auth key>; that key
    # is a secret, so never persist or print the raw bytes.
    logged_hex = "24<redacted-auth-key>" if name == "set_auth_key" else request.hex()

    await client.start_notify(OURA_NOTIFY, on_notify)
    write_capture(
        capture,
        {
            "utc": datetime.now(timezone.utc).isoformat(),
            "command": name,
            "direction": "write",
            "uuid": OURA_WRITE,
            "hex": logged_hex,
        },
    )
    print(f"write command={name} uuid={OURA_WRITE} hex={logged_hex}")
    await client.write_gatt_char(OURA_WRITE, request, response=True)
    await asyncio.sleep(listen_seconds)
    await client.stop_notify(OURA_NOTIFY)
    return responses


async def request_nonce(client: BleakClient, listen_seconds: float, capture: Optional[Path]) -> bytes:
    responses = await transact(client, COMMANDS["auth_nonce"].request, listen_seconds, capture, "auth_nonce")
    for response in responses:
        parsed = parse_packet(response)
        nonce = parsed.get("auth_nonce")
        if isinstance(nonce, str):
            return bytes.fromhex(nonce)
    raise RuntimeError("auth nonce response not received")


async def set_auth_key(client: BleakClient, key: bytes, listen_seconds: float, capture: Optional[Path]) -> None:
    await transact(client, packet(0x24, key), listen_seconds, capture, "set_auth_key")


async def authenticate(client: BleakClient, key: bytes, listen_seconds: float, capture: Optional[Path]) -> None:
    nonce = await request_nonce(client, listen_seconds, capture)
    encrypted = AES.new(key, AES.MODE_ECB).encrypt(pad(nonce, 16))
    await transact(client, packet(0x2F, bytes([0x2D]) + encrypted), listen_seconds, capture, "authenticate")


async def main() -> None:
    parser = argparse.ArgumentParser(description="Run Oura BLE protocol commands.")
    parser.add_argument("commands", nargs="*", help="Command names, 'safe-matrix', 'auth-safe-matrix', 'auth', or raw hex prefixed with hex:")
    parser.add_argument("--address", help="BLE address/platform identifier to target")
    parser.add_argument("--name-contains", default="Oura", help="Case-insensitive device name filter")
    parser.add_argument("--scan-timeout", type=float, default=30.0)
    parser.add_argument("--connect-timeout", type=float, default=60.0)
    parser.add_argument("--listen-seconds", type=float, default=2.0)
    parser.add_argument("--capture", default="captures/horizon-ring3.jsonl")
    parser.add_argument("--auth-key", help="16-byte auth key hex")
    parser.add_argument("--auth-key-file", default="captures/horizon-ring3-auth-key.hex")
    parser.add_argument("--set-auth-key", action="store_true", help="Set a new/generated auth key before auth")
    parser.add_argument("--generate-auth-key", action="store_true", help="Generate a 16-byte auth key")
    parser.add_argument("--include-state", action="store_true", help="Allow state-changing non-danger commands")
    parser.add_argument("--include-danger", action="store_true", help="Allow dangerous commands")
    parser.add_argument("--list", action="store_true", help="List known commands")
    args = parser.parse_args()

    if args.list:
        for command in COMMANDS.values():
            print(f"{command.name:28} safety={command.safety:7} request={command.request.hex()} {command.notes}")
        return

    capture = Path(args.capture) if args.capture else None
    requested = args.commands or ["safe-matrix"]
    expanded: list[tuple[str, bytes, str]] = []
    for name in requested:
        if name == "safe-matrix":
            expanded.extend((cmd, COMMANDS[cmd].request, COMMANDS[cmd].safety) for cmd in SAFE_MATRIX)
        elif name == "auth-safe-matrix":
            for cmd in AUTH_SAFE_MATRIX:
                if cmd == "auth":
                    expanded.append(("auth", b"", "safe"))
                else:
                    expanded.append((cmd, COMMANDS[cmd].request, COMMANDS[cmd].safety))
        elif name == "auth":
            expanded.append(("auth", b"", "safe"))
        elif name.startswith("hex:"):
            expanded.append((name, bytes.fromhex(name[4:]), "unknown"))
        elif name in COMMANDS:
            command = COMMANDS[name]
            request = sync_time() if command.name == "sync_time" else command.request
            expanded.append((command.name, request, command.safety))
        else:
            raise SystemExit(f"unknown command: {name}")

    for name, _, safety in expanded:
        if safety == "state" and not args.include_state:
            raise SystemExit(f"{name} is state-changing; pass --include-state")
        # Raw hex: frames are unclassified -- they can encode any opcode incl.
        # dangerous ones, so gate them behind --include-danger too.
        if safety in ("danger", "unknown") and not args.include_danger:
            raise SystemExit(f"{name} is unclassified/dangerous; pass --include-danger")

    key = load_key(args)
    if args.generate_auth_key:
        key = secrets.token_bytes(16)
        save_key(args.auth_key_file, key)
        print(f"generated_auth_key file={args.auth_key_file} hex={key.hex()}")

    candidates = await find_device(args)
    if not candidates:
        print("no_oura_candidates")
        return
    for index, (device, adv) in enumerate(candidates):
        name = device.name or adv.local_name or ""
        print(f"candidate[{index}] address={device.address} rssi={adv.rssi} name={name!r}")

    device, adv = candidates[0]
    name = device.name or adv.local_name or ""
    print(f"connecting address={device.address} rssi={adv.rssi} name={name!r}")

    async with BleakClient(device, timeout=args.connect_timeout) as client:
        print(f"connected={client.is_connected}")
        print(f"mtu_size={client.mtu_size}")
        if args.set_auth_key:
            if key is None:
                key = secrets.token_bytes(16)
                save_key(args.auth_key_file, key)
                print(f"generated_auth_key file={args.auth_key_file} (hex written to file, 0600)")
            await set_auth_key(client, key, args.listen_seconds, capture)
        for name, request, _ in expanded:
            try:
                if name == "auth":
                    if key is None:
                        raise RuntimeError("auth requires --auth-key, --auth-key-file, or --generate-auth-key")
                    await authenticate(client, key, args.listen_seconds, capture)
                else:
                    await transact(client, request, args.listen_seconds, capture, name)
            except BleakError as exc:
                print(f"bleak_error command={name} error={exc}")
            except Exception as exc:
                print(f"error command={name} type={type(exc).__name__} error={exc}")


if __name__ == "__main__":
    asyncio.run(main())
