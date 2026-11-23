#!/usr/bin/env bash
# Build the display server and the demos for the Pi (aarch64 Linux, in a
# container) and copy them there.
#   ./deploy.sh           build and copy the binary
#   ./deploy.sh service   also install and (re)start the systemd service, which
#                         drives the real LEDs from boot
#   ./deploy.sh kiosk     instead run the simulator fullscreen on the Pi's
#                         desktop from boot, showing the demos in a loop
#                         (user services display-simulator and demo-cycle)
# layout.json is only copied if it exists here; without one the Pi keeps its
# own, or the server discovers the controllers and uses one row per strip.
set -euo pipefail
cd "$(dirname "$0")"

PI=om@172.17.3.52
KEY=../claude_key
IMAGE=docker.io/library/rust:1-bookworm

podman run --rm -v "$PWD":/work:Z -v display-server-cargo:/usr/local/cargo/registry \
  -w /work "$IMAGE" sh -c "
    CARGO_TARGET_DIR=/work/target/pi cargo build --release &&
    cd demos && CARGO_TARGET_DIR=/work/demos/target/pi cargo build --release"

ssh -i "$KEY" "$PI" "mkdir -p ~/display-server/demos"
# Copy beside the running binary, then swap it in
scp -q -i "$KEY" target/pi/release/display-server "$PI:~/display-server/display-server.new"
scp -q -i "$KEY" display-server.service display-simulator.service demo-cycle.service simulate.sh \
  layout.example.json "$PI:~/display-server/"
if [ -f layout.json ]; then
  scp -q -i "$KEY" layout.json "$PI:~/display-server/"
fi
ssh -i "$KEY" "$PI" "mkdir -p ~/display-server/demos.new"
# Every demo in demos/src/bin
DEMOS=$(for f in demos/src/bin/*.rs; do basename "$f" .rs; done)
scp -q -i "$KEY" $(for d in $DEMOS; do echo demos/target/pi/release/$d; done) demos/cycle.sh "$PI:~/display-server/demos.new/"
ssh -i "$KEY" "$PI" "
  mv ~/display-server/display-server.new ~/display-server/display-server
  for d in ~/display-server/demos.new/*; do mv \$d ~/display-server/demos/; done
  rmdir ~/display-server/demos.new"
echo "installed ~/display-server/display-server and demos on $PI"

if [ "${1:-}" = service ]; then
  ssh -i "$KEY" "$PI" "
    [ -f ~/display-server/layout.json ] || echo '{}' > ~/display-server/layout.json
    sudo cp ~/display-server/display-server.service /etc/systemd/system/
    sudo systemctl daemon-reload
    sudo systemctl enable display-server
    sudo systemctl restart display-server
    sleep 5; systemctl --no-pager status display-server | head -15"
fi

if [ "${1:-}" = kiosk ]; then
  ssh -i "$KEY" "$PI" "
    if systemctl is-active -q display-server; then
      sudo systemctl disable --now display-server  # it would hold port 8080
    fi
    # So would a simulator started by hand
    systemctl --user stop demo-cycle display-simulator 2>/dev/null
    pkill -x display-server && sleep 2
    if ss -tln | grep -q ':8080 '; then echo 'port 8080 is still in use:'; ss -tlnp | grep ':8080 '; exit 1; fi
    mkdir -p ~/.config/systemd/user
    cp ~/display-server/display-simulator.service ~/display-server/demo-cycle.service ~/.config/systemd/user/
    systemctl --user daemon-reload
    systemctl --user enable display-simulator demo-cycle
    systemctl --user restart display-simulator demo-cycle
    sleep 5; systemctl --user --no-pager status display-simulator demo-cycle | grep -E 'service|Active'"
fi
