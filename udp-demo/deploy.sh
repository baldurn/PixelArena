#!/usr/bin/env bash
# Build the firmware and publish it to the Pi's OTA server.
#   ./deploy.sh        build, publish, tell the ESP32 to check for the update
#   ./deploy.sh full   also write udp-demo-full.bin (bootloader + partition
#                      table + app) for a serial flash at 0x0
set -euo pipefail
cd "$(dirname "$0")"

PI=om@172.17.3.52
KEY=../claude_key
IMAGE=docker.io/espressif/idf-rust:esp32_latest
ELF=target/xtensa-esp32-espidf/release/udp-demo

podman run --rm -v "$PWD":/work:Z \
  -v esp-cargo-registry:/home/esp/.cargo/registry \
  -v esp-idf-tools:/home/esp/.espressif \
  -w /work "$IMAGE" bash -lc "
    set -e; . ~/export-esp.sh
    cargo build --release
    espflash save-image --chip esp32 --flash-size 8mb $ELF udp-demo.bin
    if [ '${1:-}' = full ]; then
      build=\$(ls -d target/xtensa-esp32-espidf/release/build/esp-idf-sys-*/out/build | head -1)
      espflash save-image --chip esp32 --merge --flash-size 8mb \
        --bootloader \$build/bootloader/bootloader.bin \
        --partition-table partitions.csv $ELF udp-demo-full.bin
    fi"

# The image ends with its own SHA-256 digest; that is the firmware's identity
SHA=$(python3 - udp-demo.bin <<'PY'
import hashlib, sys
data = open(sys.argv[1], "rb").read()
assert hashlib.sha256(data[:-32]).digest() == data[-32:], "image has no appended digest"
print(data[-32:].hex())
PY
)
SIZE=$(stat -f %z udp-demo.bin)
BIN=udp-demo-${SHA:0:12}.bin
printf '{"sha256": "%s", "url": "/%s", "size": %s}\n' "$SHA" "$BIN" "$SIZE" > firmware.json
echo "firmware $BIN ($SIZE bytes)"

# Upload the image first, then swap the manifest in atomically
scp -q -i "$KEY" udp-demo.bin "$PI:/srv/esp-fw/$BIN"
scp -q -i "$KEY" firmware.json "$PI:/srv/esp-fw/firmware.json.tmp"
ssh -i "$KEY" "$PI" "mv /srv/esp-fw/firmware.json.tmp /srv/esp-fw/firmware.json"
scp -q -i "$KEY" esp_tool.py led_tool.py "$PI:~/"

if [ "${1:-}" != full ]; then
  ssh -i "$KEY" "$PI" "python3 ~/esp_tool.py check && python3 ~/esp_tool.py watch --expect ${SHA:0:12} --timeout 120"
fi
