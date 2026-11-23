#!/usr/bin/env python3
"""Drive WS2812 controllers over UDP: unicast frames to each, then broadcast a sync.

  led_tool.py color R G B [--strip N]  one colour (all strips, or just strip N)
  led_tool.py rainbow [--fps N]        scrolling rainbow, phase-shifted per strip
  led_tool.py chase [--fps N]          white pixel running along every strip in step
  led_tool.py show [--fps N] [--scene-time S] [--duration S]
                                       light show that jumps between strips
  led_tool.py off

Each controller has STRIPS strips of NUM_LEDS LEDs.

Controllers are found from their heartbeats (UDP 5001) unless --esp IP is given.
"""
import argparse, colorsys, math, random, socket, struct, time

LED_PORT = 5002
HEARTBEAT_PORT = 5001
BROADCAST = "192.168.50.255"
NUM_LEDS = 144
STRIPS = 4


def discover(wait=3.0):
    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    s.bind(("0.0.0.0", HEARTBEAT_PORT))
    s.settimeout(0.2)
    found, end = set(), time.time() + wait
    while time.time() < end:
        try:
            _, addr = s.recvfrom(1500)
            found.add(addr[0])
        except socket.timeout:
            pass
    return sorted(found)


class Leds:
    def __init__(self, controllers):
        self.controllers = controllers
        self.sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        self.sock.setsockopt(socket.SOL_SOCKET, socket.SO_BROADCAST, 1)
        self.frame_id = 0

    def show(self, strips):
        """strips: {strip index: list of (r, g, b)}. Same frames go to every controller;
        strips left out keep their current picture."""
        self.frame_id = (self.frame_id + 1) & 0xFFFF
        frames = [
            b"LEDF" + struct.pack(">HBBH", self.frame_id, index, 0, len(pixels)) + bytes(c for px in pixels for c in px)
            for index, pixels in strips.items()
        ]
        for ip in self.controllers:
            for frame in frames:
                self.sock.sendto(frame, (ip, LED_PORT))
        # Give the controllers time to convert the frame before the sync
        time.sleep(0.002)
        self.sock.sendto(b"LEDS" + struct.pack(">H", self.frame_id), (BROADCAST, LED_PORT))


def run(fps, render, duration=None):
    n = 0
    period = 1.0 / fps
    next_t = time.perf_counter()
    end = next_t + duration if duration else None
    try:
        while end is None or time.perf_counter() < end:
            leds.show(render(n))
            n += 1
            next_t += period
            time.sleep(max(0, next_t - time.perf_counter()))
    except KeyboardInterrupt:
        pass
    leds.show(all_strips(lambda s: [(0, 0, 0)] * NUM_LEDS))


def all_strips(render):
    return {s: render(s) for s in range(STRIPS)}


def hsv(h, v=1.0):
    return tuple(int(c * 255) for c in colorsys.hsv_to_rgb(h % 1.0, 1.0, v))


class Show:
    """Scenes that move light between strips. Draws into a float framebuffer
    that fades every frame, so moving things leave trails."""

    def __init__(self, fps, scene_time):
        self.fps = fps
        self.scene_frames = int(scene_time * fps)
        self.fb = [[[0.0, 0.0, 0.0] for _ in range(NUM_LEDS)] for _ in range(STRIPS)]
        self.scenes = [self.comet, self.hop, self.wipe, self.pingpong, self.spiral]
        self.hop_next = 0

    def fade(self, k):
        for strip in self.fb:
            for px in strip:
                px[0] *= k; px[1] *= k; px[2] *= k

    def put(self, s, i, rgb, add=False):
        if 0 <= s < STRIPS and 0 <= i < NUM_LEDS:
            px = self.fb[s][i]
            for c in range(3):
                px[c] = min(255.0, px[c] + rgb[c]) if add else rgb[c]

    def fill(self, s, rgb, start=0, end=NUM_LEDS):
        for i in range(start, end):
            self.put(s, i, rgb)

    # Comet running the strips as one serpentine chain: 0 ->, 1 <-, 2 ->, 3 <-
    def comet(self, n):
        self.fade(0.80)
        pos = (n * 3) % (STRIPS * NUM_LEDS)
        s, i = divmod(pos, NUM_LEDS)
        if s % 2:
            i = NUM_LEDS - 1 - i
        hue = n / 400
        for k in range(3):
            self.put(s, i + (k if s % 2 else -k), hsv(hue))

    # Random strip flashes a random colour on the beat
    def hop(self, n):
        self.fade(0.88)
        if n >= self.hop_next:
            self.hop_next = n + self.fps // 4
            self.fill(random.randrange(STRIPS), hsv(random.random()))

    # Colour wipe fills strip 0, 1, 2, 3 in turn, then the next colour wipes over it
    def wipe(self, n):
        per_strip = self.fps // 2
        step = n % (per_strip * STRIPS)
        s = step // per_strip
        upto = (step % per_strip + 1) * NUM_LEDS // per_strip
        self.fill(s, hsv(n // (per_strip * STRIPS) * 0.27), 0, upto)

    # Whole-strip block bouncing 0 1 2 3 2 1 0 ... with fading trail
    def pingpong(self, n):
        self.fade(0.75)
        period = self.fps // 8
        seq = list(range(STRIPS)) + list(range(STRIPS - 2, 0, -1))
        s = seq[(n // period) % len(seq)]
        self.fill(s, hsv(n / 300))

    # Band that rotates across the strips at an angle: each strip lags the previous
    def spiral(self, n):
        self.fade(0.6)
        for s in range(STRIPS):
            centre = (n * 2 - s * NUM_LEDS // STRIPS) % NUM_LEDS
            for k in range(-6, 7):
                level = math.cos(k / 6 * math.pi / 2)
                r, g, b = hsv(s / STRIPS + n / 500)
                self.put(s, (centre + k) % NUM_LEDS, (r * level, g * level, b * level), add=True)

    def render(self, n):
        scene = self.scenes[(n // self.scene_frames) % len(self.scenes)]
        if n % self.scene_frames == 0:
            print("scene:", scene.__name__, flush=True)
            self.fade(0)
        scene(n)
        return {s: [tuple(int(c) for c in px) for px in self.fb[s]] for s in range(STRIPS)}


p = argparse.ArgumentParser()
p.add_argument("--esp", action="append", help="controller IP (repeatable); default: discover")
sub = p.add_subparsers(dest="cmd", required=True)
c = sub.add_parser("color"); [c.add_argument(x, type=int) for x in "rgb"]
c.add_argument("--strip", type=int)
for name in ("rainbow", "chase"):
    sub.add_parser(name).add_argument("--fps", type=float, default=40)
sh = sub.add_parser("show")
sh.add_argument("--fps", type=int, default=40)
sh.add_argument("--scene-time", type=float, default=10)
sh.add_argument("--duration", type=float, help="stop after S seconds (default: run until Ctrl-C)")
sub.add_parser("off")
a = p.parse_args()

controllers = a.esp or discover()
if not controllers:
    raise SystemExit("no controllers found (no heartbeats on UDP 5001)")
print("controllers:", ", ".join(controllers))
leds = Leds(controllers)

if a.cmd == "color":
    pixels = [(a.r, a.g, a.b)] * NUM_LEDS
    leds.show({a.strip: pixels} if a.strip is not None else all_strips(lambda s: pixels))
elif a.cmd == "off":
    leds.show(all_strips(lambda s: [(0, 0, 0)] * NUM_LEDS))
elif a.cmd == "rainbow":
    run(a.fps, lambda n: all_strips(lambda s: [hsv(i / NUM_LEDS + n / 200 + s / STRIPS) for i in range(NUM_LEDS)]))
elif a.cmd == "chase":
    run(a.fps, lambda n: all_strips(lambda s: [(255, 255, 255) if i == n % NUM_LEDS else (0, 0, 0) for i in range(NUM_LEDS)]))
elif a.cmd == "show":
    run(a.fps, Show(a.fps, a.scene_time).render, a.duration)
