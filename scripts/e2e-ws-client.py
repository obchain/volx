#!/usr/bin/env python3
"""End-to-end WebSocket client used by `e2e-smoke.sh` (issue #66).

Connects to `ws://localhost:8080/v1/stream`, subscribes to both `bvol` and
`evol`, and asserts that at least one tick of each channel arrives inside
a fixed window.

Exit codes:
  0 — both channels delivered at least one tick.
  1 — at least one channel was silent (or the WS handshake failed).

The frame contract is fixed by `api/internal/stream/hub.go`:

    {"type": "tick", "channel": "bvol", "value": 37.37,
     "ts": 1779782592444, "confidence": 1.0}

Usage:
  python3 scripts/e2e-ws-client.py [--url URL] [--timeout SECONDS]
"""
from __future__ import annotations

import argparse
import asyncio
import json
import sys
from typing import Any

import websockets

REQUIRED_CHANNELS = {"bvol", "evol"}
DEFAULT_URL = "ws://localhost:8080/v1/stream"
DEFAULT_TIMEOUT = 75.0


def _validate(frame: dict[str, Any]) -> bool:
    """Strict wire-shape check against PRD §6."""
    if frame.get("type") != "tick":
        return False
    if frame.get("channel") not in REQUIRED_CHANNELS:
        return False
    for k in ("value", "confidence"):
        if not isinstance(frame.get(k), (int, float)):
            return False
    if not isinstance(frame.get("ts"), int):
        return False
    return True


async def run(url: str, timeout: float) -> int:
    seen: dict[str, dict[str, Any]] = {}
    print(f"[ws] connect {url}", flush=True)
    try:
        async with websockets.connect(url, ping_interval=20, ping_timeout=15) as ws:
            await ws.send(json.dumps({"action": "subscribe", "channels": list(REQUIRED_CHANNELS)}))
            print("[ws] subscribed; awaiting ticks", flush=True)

            async def consume() -> None:
                async for raw in ws:
                    try:
                        frame = json.loads(raw)
                    except json.JSONDecodeError:
                        print(f"[ws] non-json frame: {raw!r}", flush=True)
                        continue
                    if frame.get("type") == "error":
                        print(f"[ws] server error: {frame}", flush=True)
                        continue
                    if not _validate(frame):
                        print(f"[ws] malformed tick: {frame}", flush=True)
                        continue
                    ch = frame["channel"]
                    if ch not in seen:
                        seen[ch] = frame
                        print(
                            f"[ws] tick {ch} value={frame['value']:.4f} "
                            f"ts={frame['ts']} confidence={frame['confidence']}",
                            flush=True,
                        )
                    if REQUIRED_CHANNELS.issubset(seen.keys()):
                        return

            try:
                await asyncio.wait_for(consume(), timeout=timeout)
            except asyncio.TimeoutError:
                pass
    except (OSError, websockets.exceptions.WebSocketException) as e:
        print(f"[ws] handshake/transport failure: {e}", file=sys.stderr, flush=True)
        return 1

    missing = REQUIRED_CHANNELS - seen.keys()
    if missing:
        print(
            f"[ws] FAIL — channels with no tick in {timeout:.0f}s window: {sorted(missing)}",
            file=sys.stderr,
            flush=True,
        )
        return 1

    print(f"[ws] OK — both channels delivered (saw {sorted(seen.keys())})", flush=True)
    return 0


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--url", default=DEFAULT_URL)
    ap.add_argument("--timeout", type=float, default=DEFAULT_TIMEOUT)
    args = ap.parse_args()
    return asyncio.run(run(args.url, args.timeout))


if __name__ == "__main__":
    sys.exit(main())
