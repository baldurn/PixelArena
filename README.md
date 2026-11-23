# PixelArena

An LED display of vertical pipes standing in a circle, driven by ESP32
controllers and a Raspberry Pi, with games and demos written against a
simple WebSocket protocol.

- **display-server/**: the Pi's display server. Games connect over a
  WebSocket and send frames; the server documents its own protocol at
  `http://<pi>:8080/`, so a game can be written by handing that URL to an AI.
  It also takes DDP from WLED and xLights, relays two players' keyboard input
  to games, and can show the display in a simulator window instead of on
  the LEDs.
  - **demos/**: demos and the two-player game Duel, plus `cycle.sh`, which
    shows them in a loop.
  - `deploy.sh` builds for the Pi and installs; `deploy.sh kiosk` runs the
    simulator fullscreen with the demo loop from boot.
- **udp-demo/**: firmware for the WT32-ETH01 LED controllers (Ethernet, four
  strips each, frames over UDP, over-the-air updates).
- **blink/**, **ws2812/**: first experiments on the ESP32.

The deploy scripts expect an SSH key for the Pi at `./claude_key`, which is
not in the repository.
