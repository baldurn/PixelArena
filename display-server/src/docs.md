# LED Display Server — protocol for games (version $protocol)

This server drives an LED display. A game connects over a WebSocket, sends
picture frames, and can play sounds. This page is the complete specification:
everything needed to write a game is here. Live values on this page (display
size, frame rate, URLs) are filled in by the running server.

## The display

- Size: **$width x $height pixels** (width x height). Each pixel is one RGB LED.
- Coordinates: x = 0 is the first column, y = 0 is the top row. The last
  pixel is ($last_x, $last_y). See "Physical shape" for where the columns are.
- Frame rate: the display refreshes **$fps times per second**.

### Physical shape

$physical_text

### Look

- This is a very low resolution display seen from a distance: use bold shapes,
  solid colours and motion. Text is only readable if drawn very large, if at all.
- Brightness: the controllers scale down frames that would draw too much
  power, so many full-white pixels make the whole picture dimmer. Saturated
  colours on a black background look best. Black (0, 0, 0) is off.

## Connecting

Open a WebSocket to:

    ws://$host/ws

Any WebSocket client library works. The server only speaks plain `ws://` (no TLS).

The server immediately sends a `welcome` message (see below). Then your game
sends `hello` with its name. Only one game is connected at a time: **the most
recently connected game takes over the display**, and the game that was
running gets a `replaced` message and is disconnected. When no game is
connected the display shows an idle animation. To quit, just close the
connection.

## Message types

- **Binary WebSocket messages** are picture frames (game -> server).
- **Text WebSocket messages** are JSON objects with a `"type"` field (both directions).

### Frames (binary, game -> server)

A frame is exactly **$frame_bytes bytes**: $width x $height pixels, 3 bytes per
pixel in the order R, G, B (0-255 each), rows from top to bottom, each row left
to right. The byte offset of pixel (x, y) is `(y * $width + x) * 3`.

The server shows the most recent frame it has received at each tick; if you
send nothing, the last frame stays on the display. A frame of the wrong size is
rejected with an `error` message. Until your first frame arrives the display
is black.

### Server -> game

`welcome` — sent once, right after connecting:

    {"type": "welcome", "protocol": $protocol,
     "display": {"width": $width, "height": $height, "fps": $fps, "pixel_format": "rgb24",
                 "frame_bytes": $frame_bytes, "brightness": 1.0, "gamma": 1.0,
                 "physical": $physical_json},
     "audio": {"enabled": true, "sample_rate": $sample_rate, "max_clip_seconds": $max_clip,
               "waves": ["sine", "square", "triangle", "saw", "noise"]},
     "input": {"enabled": true, "players": 2,
               "buttons": ["up", "down", "left", "right", "fire"]}}

Always read the display size from `welcome` instead of hard-coding it: the
display may be rebuilt with a different size.

`tick` — sent every time the display refreshes ($fps per second), with an
increasing frame counter. The simplest game loop is: wait for a tick, update
the game, send one frame.

    {"type": "tick", "frame": 1234}

`input` — a player pressed or released a button. Player input is
$input_state.

    {"type": "input", "player": 1, "button": "left", "pressed": true}
    {"type": "input", "player": 1, "button": "left", "pressed": false}

- `player`: 1 or 2. Player 1 uses the arrow keys and Enter, player 2 uses
  W A S D and Space.
- `button`: `up`, `down`, `left`, `right` or `fire`.
- `pressed`: true when the button goes down, false when it is let go. There is
  no key repeat: a held button sends one `pressed: true`, and nothing more
  until it is released.

Keep a table of which buttons are held and update it from these messages; read
it on each `tick` to move things. For actions that happen once per press, such
as firing, act on `pressed: true` itself, so a press and release between two
ticks is not lost. Players may not be there at all: a game should still look
good with nobody playing, for instance with the computer playing a side
until someone presses a button.

`replaced` — another game connected and took over the display. The server
then closes the connection normally (close code 1000), so a receive loop
like the one in the example below just ends. The game should exit;
reconnecting takes the display back.

    {"type": "replaced", "message": "another game connected and took over the display"}

`error` — something your game sent was rejected. The connection stays open.

    {"type": "error", "error": "bad_frame_size", "message": "frame is 100 bytes, expected $frame_bytes ..."}

Error codes: `bad_frame_size`, `bad_json`, `unknown_type`, `bad_message`. The
`message` text explains what to fix. Errors are rate-limited, so not every bad
message gets its own error.

`sound_loaded` — reply to `sound_load`:

    {"type": "sound_loaded", "id": "explosion", "seconds": 0.82}

### Game -> server

`hello` — your game's name, shown in the server's status. Send it first.

    {"type": "hello", "name": "Snake"}

`tone` — play a generated sound effect right away. Best for retro bleeps.

    {"type": "tone", "wave": "square", "freq": 440, "ms": 100, "volume": 0.5}
    {"type": "tone", "wave": "saw", "freq": 1200, "to_freq": 200, "ms": 300}

- `wave`: one of `sine`, `square`, `triangle`, `saw`, `noise` (default `square`).
- `freq`: start frequency in Hz (20-10000). `to_freq` (optional): glide
  linearly to this frequency over the tone, for lasers, jumps and falls.
- `ms`: duration, 1-5000 (default 100). `volume`: 0-1 (default 1).
- `id` (optional): a name, so `sound_stop` can stop it early.

`sound_load` — upload a sound clip once, then play it by id as often as you like:

    {"type": "sound_load", "id": "explosion", "data": "<base64 of a .wav file>"}

The clip must be a PCM WAV file, 8- or 16-bit, mono or stereo, any sample rate
(it is converted to mono $sample_rate Hz), at most $max_clip seconds. Loading a
clip under an existing id replaces it.

`sound_play` — play a loaded clip. Several sounds can play at once.

    {"type": "sound_play", "id": "explosion", "volume": 0.8}
    {"type": "sound_play", "id": "music", "volume": 0.3, "loop": true}

`sound_stop` — stop all playing copies of one sound, or every sound of your
game when `id` is left out:

    {"type": "sound_stop", "id": "music"}
    {"type": "sound_stop"}

A game's sounds stop when it disconnects or is replaced.
Audio on this server is $audio_state.

## Other sources: DDP (WLED, xLights)

$ddp_text

## Example game (Python)

Needs the `websockets` package, version 11 or newer (`pip install websockets`).
A dot bounces around the display and beeps at the edges.

```python
import json
from websockets.sync.client import connect

with connect("ws://$host/ws") as ws:
    welcome = json.loads(ws.recv())
    W, H = welcome["display"]["width"], welcome["display"]["height"]
    ws.send(json.dumps({"type": "hello", "name": "Bouncer"}))

    x, y, dx, dy = 0, 0, 1, 1
    for raw in ws:
        msg = json.loads(raw)
        if msg["type"] == "error":
            print("server error:", msg["message"])
        if msg["type"] != "tick":
            continue
        x, y = x + dx, y + dy
        if not 0 <= x < W:
            dx, x = -dx, x - 2 * dx
            ws.send(json.dumps({"type": "tone", "freq": 660, "ms": 40, "volume": 0.4}))
        if not 0 <= y < H:
            dy, y = -dy, y - 2 * dy
            ws.send(json.dumps({"type": "tone", "freq": 440, "ms": 40, "volume": 0.4}))
        frame = bytearray(W * H * 3)
        frame[(y * W + x) * 3:(y * W + x) * 3 + 3] = bytes((255, 80, 0))
        ws.send(bytes(frame))
```

## Tips

- Build the frame as one `W * H * 3` byte buffer and send it as a single binary
  message per tick. Do not send JSON pixel lists.
- Keep the game's own timing tied to `tick` messages, so it runs at the
  display's speed and does not flood the server.
- `GET http://$host/info` shows the server's status (display size, the game
  that is connected).
- Player input arrives as `input` messages (see above); `welcome` says in
  `input.enabled` whether this server has any.
