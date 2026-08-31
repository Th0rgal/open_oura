#!/usr/bin/env node
/*
 * Extract an Oura Android DbRingConfiguration auth_key from assa-store.realm.
 *
 * Requires the npm `realm` package:
 *   npm install realm@20
 *
 * The key is written to a 0600 file and is never printed.
 */

const fs = require("fs");
let Realm;
try {
  Realm = require("realm");
} catch {
  Realm = require(require.resolve("realm", { paths: [process.cwd()] }));
}

function usage() {
  console.error(
    "usage: extract_oura_realm_key.js <assa-store.realm> <serial> <out-key-file>"
  );
  process.exit(2);
}

const [realmPath, serial, outPath] = process.argv.slice(2);
if (!realmPath || !serial || !outPath) usage();

const realm = new Realm.Realm({ path: realmPath, readOnly: true });
try {
  const rows = Array.from(realm.objects("DbRingConfiguration"));
  const matching = rows.filter((row) => row.serial_number === serial);
  if (matching.length === 0) {
    throw new Error(`no DbRingConfiguration row for serial ${serial}`);
  }

  const active =
    matching.find((row) => row.in_active_use === true && row.deleted_at == null) ||
    matching.find((row) => row.deleted_at == null) ||
    matching[0];

  if (!active.auth_key || active.auth_key.byteLength !== 16) {
    throw new Error(`DbRingConfiguration.auth_key is missing or not 16 bytes`);
  }

  const hex = Buffer.from(active.auth_key).toString("hex");
  fs.writeFileSync(outPath, `${hex}\n`, { mode: 0o600 });
  fs.chmodSync(outPath, 0o600);

  console.log(
    JSON.stringify({
      wrote: outPath,
      serial: active.serial_number,
      mac_address: active.mac_address,
      android_ble_identifier: active.android_ble_identifier,
      auth_key_bytes: active.auth_key.byteLength,
    })
  );
} finally {
  realm.close();
}

process.exit(0);
