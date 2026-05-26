"use client";

import { useEffect, useRef, useState } from "react";
import { WS_URL, type IndexId, type TickFrame, type WsFrame } from "./api";

export type ConnState = "connecting" | "open" | "closed";

export interface Tick {
  value: number;
  ts: number;
  confidence: number;
}

// Subscribes to /v1/stream once on mount; reconnects with exponential
// backoff on drop. Returns the latest tick per channel + connection
// status. Components should NOT instantiate more than one of these per
// page — fan-out from a single subscription is cheaper.
export function useIndexTicks(channels: IndexId[]) {
  const [state, setState] = useState<ConnState>("connecting");
  const [ticks, setTicks] = useState<Record<IndexId, Tick | null>>({
    bvol: null,
    evol: null,
  });

  // Keep the channel list stable across renders for the reconnect loop.
  const channelsRef = useRef(channels);
  channelsRef.current = channels;

  useEffect(() => {
    let ws: WebSocket | null = null;
    let backoff = 500;
    let stopped = false;
    let reconnectTimer: ReturnType<typeof setTimeout> | null = null;

    const connect = () => {
      if (stopped) return;
      setState("connecting");
      ws = new WebSocket(WS_URL);

      ws.onopen = () => {
        if (!ws) return;
        backoff = 500;
        setState("open");
        ws.send(
          JSON.stringify({
            action: "subscribe",
            channels: channelsRef.current,
          }),
        );
      };

      ws.onmessage = (ev) => {
        try {
          const frame = JSON.parse(ev.data as string) as WsFrame;
          if (frame.type === "tick") {
            const t = frame as TickFrame;
            setTicks((prev) => ({
              ...prev,
              [t.channel]: {
                value: t.value,
                ts: t.ts,
                confidence: t.confidence,
              },
            }));
          }
        } catch {
          // ignore malformed frames
        }
      };

      ws.onclose = () => {
        if (stopped) return;
        setState("closed");
        backoff = Math.min(backoff * 2, 15_000);
        reconnectTimer = setTimeout(connect, backoff);
      };

      ws.onerror = () => {
        ws?.close();
      };
    };

    connect();

    return () => {
      stopped = true;
      if (reconnectTimer) clearTimeout(reconnectTimer);
      ws?.close();
    };
  }, []);

  return { state, ticks };
}
