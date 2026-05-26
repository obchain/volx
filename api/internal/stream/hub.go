// Package stream implements the live tick broadcast layer per PRD §6
// and issue #24. The pipeline shape:
//
//	Engine → Redis PUBLISH index:{id}:stream ─┐
//	                                          ▼
//	                                       Hub.Run (one PSUBSCRIBE for all indices)
//	                                          │
//	                                          ▼
//	                            fan-out → registered Conn.send channels
//	                                          │
//	                                          ▼
//	                              gorilla WS frames → browser
//
// One [`Hub`] runs per API process. It subscribes to the pattern
// `index:*:stream` and pushes each parsed tick to every connection
// whose `channels` set includes the index.
//
// Wire format (PRD §6, line 988):
//
//	// client → server
//	{ "action": "subscribe", "channels": ["bvol", "evol"] }
//
//	// server → client (per tick)
//	{ "type":"tick", "channel":"bvol",
//	  "value": 67.42, "ts": 1747668092847, "confidence": 0.97 }
//
// `ts` is Unix epoch **milliseconds** (integer) — different from the
// REST endpoints' RFC 3339 form. PRD lines 1054–1056 show the
// frontend doing `tick.ts / 1000` for `lightweight-charts`'
// seconds-since-epoch input.
package stream

import (
	"context"
	"encoding/json"
	"errors"
	"strings"
	"sync"
	"time"

	"github.com/redis/go-redis/v9"
)

// pubsubPattern matches every `index:{id}:stream` channel the engine
// publishes to. One subscriber per process keeps the Redis read cost
// constant regardless of how many WS clients connect.
const pubsubPattern = "index:*:stream"

// SendQueueDepth bounds the per-connection outbound buffer. A slow
// WS client (high-latency network, paused browser tab) cannot wedge
// the hub — when the queue is full the *oldest* message is dropped
// and a `volx_api_ws_dropped_total{reason="slow_consumer"}` counter
// would increment (counter wired when #11 engine exporter symmetry
// lands).
const SendQueueDepth = 32

// PingInterval governs keepalive ping cadence. A client that does
// not respond with a pong inside `PingInterval + PongTimeout` is
// closed. 30 s is well under common proxy idle timeouts (60-120 s).
const PingInterval = 30 * time.Second

// PongTimeout is how long after a ping we wait for the pong.
const PongTimeout = 10 * time.Second

// WriteTimeout caps how long a single data-frame write can block
// before we declare the WS connection dead. Distinct from
// `PongTimeout` so the keepalive cadence + per-frame stall budget
// can be tuned independently.
const WriteTimeout = 10 * time.Second

// EngineTick is the JSON envelope the engine writes to Redis. Same
// shape as `handlers.engineIndexValue` but kept independent because
// the two are wired-time contracts that may diverge in future (e.g.
// stream adds a sequence number that REST doesn't).
type EngineTick struct {
	IndexID    string  `json:"index_id"`
	Value      float64 `json:"value"`
	Confidence float64 `json:"confidence"`
	StripHash  string  `json:"strip_hash"`
	Ts         string  `json:"ts"`
}

// ClientTick is the wire shape pushed to browsers. Channel name is
// **lowercase** (`bvol`) to match the URL convention; `ts` is Unix
// epoch milliseconds (integer) per PRD §6.
type ClientTick struct {
	Type       string  `json:"type"`
	Channel    string  `json:"channel"`
	Value      float64 `json:"value"`
	Ts         int64   `json:"ts"`
	Confidence float64 `json:"confidence"`
}

// Hub fans Redis pubsub messages out to registered connections.
// Safe for concurrent use.
type Hub struct {
	redis *redis.Client

	mu    sync.RWMutex
	conns map[*Conn]struct{}
}

// NewHub returns an unstarted hub. Call `Run` from a goroutine to
// begin the Redis subscribe loop.
func NewHub(r *redis.Client) *Hub {
	return &Hub{
		redis: r,
		conns: make(map[*Conn]struct{}),
	}
}

// Run subscribes to the pattern and forwards each message to the
// registered connections. Blocks until `ctx` cancels or the
// pubsub channel closes. The hub never returns an error — pubsub
// failures are surfaced via `volx_api_ws_pubsub_errors_total` in a
// future observability PR.
func (h *Hub) Run(ctx context.Context) {
	psub := h.redis.PSubscribe(ctx, pubsubPattern)
	defer func() { _ = psub.Close() }()

	ch := psub.Channel()
	for {
		select {
		case <-ctx.Done():
			return
		case msg, ok := <-ch:
			if !ok {
				return
			}
			h.fanOut(msg.Channel, msg.Payload)
		}
	}
}

// channelToIndex parses `index:{id}:stream` into the lowercase
// `bvol|evol` form. Returns ok=false on any other pattern (engine
// might publish to other channels in future; we ignore them safely).
func channelToIndex(redisChannel string) (string, bool) {
	if !strings.HasPrefix(redisChannel, "index:") || !strings.HasSuffix(redisChannel, ":stream") {
		return "", false
	}
	mid := strings.TrimSuffix(strings.TrimPrefix(redisChannel, "index:"), ":stream")
	switch mid {
	case "BVOL":
		return "bvol", true
	case "EVOL":
		return "evol", true
	default:
		return "", false
	}
}

// fanOut parses one Redis message and pushes it to subscribed conns.
// Decoding lives here (once per pubsub message) rather than per
// connection (once per recipient) — the latter would do
// `N_conns × M_msgs/sec` JSON parses.
func (h *Hub) fanOut(redisChannel, payload string) {
	channel, ok := channelToIndex(redisChannel)
	if !ok {
		return
	}

	var et EngineTick
	if err := json.Unmarshal([]byte(payload), &et); err != nil {
		return
	}

	// RFC 3339 (engine) → epoch ms (wire). Fall back to "now" if
	// the engine ts is malformed — a client tick missing `ts`
	// would break the frontend chart axis worse than a slightly
	// off timestamp.
	var tsMs int64
	if t, err := time.Parse(time.RFC3339Nano, et.Ts); err == nil {
		tsMs = t.UnixMilli()
	} else {
		tsMs = time.Now().UnixMilli()
	}
	ct := ClientTick{
		Type:       "tick",
		Channel:    channel,
		Value:      et.Value,
		Ts:         tsMs,
		Confidence: et.Confidence,
	}
	encoded, err := json.Marshal(ct)
	if err != nil {
		return
	}

	h.mu.RLock()
	defer h.mu.RUnlock()
	for c := range h.conns {
		if c.subscribed(channel) {
			c.tryPush(encoded)
		}
	}
}

// Register adds a connection to the fan-out set. Idempotent.
func (h *Hub) Register(c *Conn) {
	h.mu.Lock()
	h.conns[c] = struct{}{}
	h.mu.Unlock()
}

// Unregister removes the connection. Idempotent (extra calls are
// no-ops, so the per-conn defer is always safe even if the conn was
// never successfully registered).
func (h *Hub) Unregister(c *Conn) {
	h.mu.Lock()
	delete(h.conns, c)
	h.mu.Unlock()
}

// ErrUnknownChannel is returned by `Conn.subscribeTo` when the
// client requests a channel that does not map to an index.
var ErrUnknownChannel = errors.New("unknown channel (allowed: bvol, evol)")
