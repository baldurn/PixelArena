#!/usr/bin/env python3
"""Pi-side helper for the ESP32 UDP demo.

  esp_tool.py watch [--expect FW] [--timeout S]  print heartbeats (until FW is seen)
  esp_tool.py check                              ask the ESP32 to check for an OTA update
  esp_tool.py echo [N]                           round-trip N echo requests
"""
import argparse, socket, sys, time

ECHO_PORT = 5000
HEARTBEAT_PORT = 5001
BROADCAST = ("192.168.50.255", ECHO_PORT)


def heartbeats(timeout):
    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    s.bind(("0.0.0.0", HEARTBEAT_PORT))
    s.settimeout(timeout)
    while True:
        data, addr = s.recvfrom(1500)
        yield addr[0], data.decode(errors="replace")


def watch(expect, timeout):
    deadline = time.time() + timeout
    try:
        for ip, msg in heartbeats(timeout):
            print(f"{ip}: {msg}", flush=True)
            if expect and f"fw {expect}" in msg:
                print(f"ESP32 at {ip} is running firmware {expect}")
                return 0
            if time.time() > deadline:
                break
    except socket.timeout:
        pass
    print("timed out" + (f" waiting for firmware {expect}" if expect else ""))
    return 1 if expect else 0


def check():
    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    s.setsockopt(socket.SOL_SOCKET, socket.SO_BROADCAST, 1)
    s.settimeout(2)
    s.sendto(b"check", BROADCAST)
    try:
        data, addr = s.recvfrom(1500)
        print(f"{addr[0]}: {data.decode()}")
        return 0
    except socket.timeout:
        print("no reply to check request")
        return 1


def echo(count):
    ip, msg = next(heartbeats(5))
    print(f"found ESP32 at {ip} ({msg})")
    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    s.settimeout(1)
    rtts = []
    for i in range(count):
        msg = f"hello from Pi #{i + 1}"
        t0 = time.perf_counter()
        s.sendto(msg.encode(), (ip, ECHO_PORT))
        try:
            data, _ = s.recvfrom(1500)
        except socket.timeout:
            print(f"  {msg!r}: timeout")
            continue
        rtt = (time.perf_counter() - t0) * 1000
        rtts.append(rtt)
        print(f"  {msg!r} -> {data.decode()!r} ({rtt:.2f} ms)")
    print(f"{len(rtts)}/{count} replies" + (f", avg RTT {sum(rtts) / len(rtts):.2f} ms" if rtts else ""))
    return 0 if len(rtts) == count else 1


p = argparse.ArgumentParser()
sub = p.add_subparsers(dest="cmd", required=True)
w = sub.add_parser("watch")
w.add_argument("--expect")
w.add_argument("--timeout", type=float, default=30)
sub.add_parser("check")
e = sub.add_parser("echo")
e.add_argument("count", nargs="?", type=int, default=5)
a = p.parse_args()
sys.exit({"watch": lambda: watch(a.expect, a.timeout), "check": check, "echo": lambda: echo(a.count)}[a.cmd]())
