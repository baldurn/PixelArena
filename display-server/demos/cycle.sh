#!/usr/bin/env bash
# Shows the demos in turn, forever, each for a while.
#   ./cycle.sh [SECONDS] [DEMO ...]
#   SECONDS: how long each demo runs (default 60)
#   DEMO: which demos, in order (default: every demo next to this script)
# When a player presses a button, GAME (default duel) starts right away; it
# runs while people play and the cycle goes on once nobody has for 30 s.
# Set SERVER to use another display server (default localhost:8080).
#
# When another game or a DDP stream (WLED) takes over the display, the demo
# exits with status 2 and the cycle waits until the display has been free
# for RESUME_AFTER seconds in a row (default 30) instead of taking it back.
# A WLED stream that pauses now and then keeps the display that way.
set -u
cd "$(dirname "$0")"
SECONDS_EACH=${1:-60}
shift || true
SERVER=${SERVER:-localhost:8080}
RESUME_AFTER=${RESUME_AFTER:-30}
GAME=${GAME:-duel}
if [ $# -gt 0 ]; then
  DEMOS=("$@")
else
  DEMOS=()
  for f in *; do
    [ -f "$f" ] && [ -x "$f" ] && [ "$f" != "$(basename "$0")" ] && DEMOS+=("$f")
  done
fi
[ ${#DEMOS[@]} -gt 0 ] || { echo "cycle: no demos found" >&2; exit 1; }

# True while a game other than ours or a DDP stream has the display
busy() {
  curl -fs "http://$SERVER/info" | grep -qE '"(game|source)": "'
}

# Returns once nobody else has used the display for RESUME_AFTER seconds
wait_until_free() {
  local free=0
  while [ $free -lt "$RESUME_AFTER" ]; do
    if busy; then free=0; else free=$((free + 2)); fi
    sleep 2
  done
}

echo "cycle: ${DEMOS[*]}, ${SECONDS_EACH}s each, on $SERVER"
while true; do
  for demo in "${DEMOS[@]}"; do
    ./"$demo" "ws://$SERVER/ws" --timeout "$SECONDS_EACH" --exit-on-input
    status=$?
    if [ $status -eq 3 ] && [ -x "./$GAME" ]; then
      echo "cycle: a player pressed a button, starting $GAME"
      # Gives the players 30 s to start; after that it runs while they play
      ./"$GAME" "ws://$SERVER/ws" --timeout 30
      status=$?
    fi
    if [ $status -eq 2 ]; then
      echo "cycle: display taken over; resuming once it has been free for ${RESUME_AFTER}s"
      wait_until_free
      echo "cycle: display free, resuming"
    elif [ $status -ne 0 ]; then
      # Server down or restarting: do not spin
      sleep 5
    fi
  done
done
