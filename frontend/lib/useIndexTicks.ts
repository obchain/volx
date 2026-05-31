"use client";

import { createContext, createElement, useContext, useEffect, useState, type ReactNode } from "react";
import { WS_URL, type IndexId, type TickFrame, type WsFrame } from "./api";

export type ConnState = "connecting" | "open" | "closed";

export interface Tick {
  value: number;
  ts: number;
  confidence: number;
}

interface IndexTicksValue {
  state: ConnState;
  ticks: Record<IndexId, Tick | null>;
}

const ALL_CHANNELS: IndexId[] = ["bvol", "evol"];

const Ctx = createContext<IndexTicksValue | null>(null);

// Single WebSocket for the whole app: opens one /v1/stream connection,
// subscribes to every channel, and fans the latest tick per channel out via
// context. Every consumer shares this one connection — avoids the per-component
// WS proliferation that would blow past WS_MAX_CONNS_PER_IP.
export function IndexTicksProvider({ children }: { children: ReactNode }) {
  const [state, setState] = useState<ConnState>("connecting");
  const [ticks, setTicks] = useState<Record<IndexId, Tick | null>>({ bvol: null, evol: null });

  useEffect(() => {
    let ws: WebSocket | null = null;
    let backoff = 500;
    let stopped = false;
    let reconnectTimer: ReturnType<typeof setTimeout> | null = null;

    const scheduleReconnect = () => {
      if (stopped) return;
      setState("closed");
      backoff = Math.min(backoff * 2, 15_000);
      reconnectTimer = setTimeout(connect, backoff);
    };

    function connect() {
      if (stopped) return;
      setState("connecting");
      try {
        ws = new WebSocket(WS_URL);
      } catch {
        // e.g. ws:// blocked from an https page (misconfigured base) — back off
        // instead of throwing out of the effect.
        scheduleReconnect();
        return;
      }

      ws.onopen = () => {
        if (!ws) return;
        backoff = 500;
        setState("open");
        ws.send(JSON.stringify({ action: "subscribe", channels: ALL_CHANNELS }));
      };

      ws.onmessage = (ev) => {
        try {
          const frame = JSON.parse(ev.data as string) as WsFrame;
          if (frame.type === "tick") {
            const t = frame as TickFrame;
            setTicks((prev) => ({ ...prev, [t.channel]: { value: t.value, ts: t.ts, confidence: t.confidence } }));
          }
        } catch {
          // ignore malformed frames
        }
      };

      ws.onclose = scheduleReconnect;
      ws.onerror = () => ws?.close();
    }

    connect();

    return () => {
      stopped = true;
      if (reconnectTimer) clearTimeout(reconnectTimer);
      ws?.close();
    };
  }, []);

  return createElement(Ctx.Provider, { value: { state, ticks } }, children);
}

// Consume the shared tick stream. The `channels` argument is advisory (the
// provider always subscribes to all channels); kept for call-site
// compatibility — consumers just read the channels they care about from `ticks`.
export function useIndexTicks(_channels?: IndexId[]): IndexTicksValue {
  const v = useContext(Ctx);
  if (!v) throw new Error("useIndexTicks must be used within IndexTicksProvider");
  return v;
}
