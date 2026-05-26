package stream

import (
	"encoding/json"
	"log/slog"
	"net/http"
	"sync"
	"time"

	"github.com/gorilla/websocket"
)

// MaxClientMsgBytes caps the size of a single client → server frame.
// Clients only send the small `subscribe` envelope (~50 B); a larger
// frame is either a misuse or an attempt to allocate-spam the
// server.
const MaxClientMsgBytes = 4 * 1024

// upgrader configures the gorilla WS handshake. Origin check is
// permissive at v1 (the API is read-only, no cookie auth, no CSRF
// surface). Tightening lands when paid-tier auth ships in #M3.
var upgrader = websocket.Upgrader{
	ReadBufferSize:  1024,
	WriteBufferSize: 1024,
	CheckOrigin: func(_ *http.Request) bool {
		return true
	},
}

// Conn is one client WebSocket. The hub holds a *Conn pointer to
// fan messages into `send`; the reader + writer goroutines own all
// other lifecycle.
type Conn struct {
	ws   *websocket.Conn
	send chan []byte

	mu       sync.RWMutex
	channels map[string]struct{}

	closeOnce sync.Once
	closed    chan struct{}
}

// newConn wraps a freshly-upgraded gorilla connection.
func newConn(ws *websocket.Conn) *Conn {
	return &Conn{
		ws:       ws,
		send:     make(chan []byte, SendQueueDepth),
		channels: make(map[string]struct{}),
		closed:   make(chan struct{}),
	}
}

// subscribed reports whether this connection wants messages for the
// given lowercase index id.
func (c *Conn) subscribed(channel string) bool {
	c.mu.RLock()
	_, ok := c.channels[channel]
	c.mu.RUnlock()
	return ok
}

// tryPush hands a pre-encoded frame to the writer goroutine.
// Drop-newest on a full queue — the wedged-slow-client case. The
// alternative (block-the-hub) would create head-of-line blocking
// across all conns.
func (c *Conn) tryPush(frame []byte) {
	select {
	case c.send <- frame:
	default:
		// Slow consumer; drop newest. A `slow_consumer` metric
		// would increment here when the API exporter symmetry
		// PR lands.
	}
}

// close terminates the connection and unblocks both goroutines.
// Idempotent (sync.Once).
func (c *Conn) close() {
	c.closeOnce.Do(func() {
		close(c.closed)
		_ = c.ws.Close()
	})
}

// --- client → server message ---------------------------------------

// clientMsg is the only inbound envelope shape we accept. PRD §6
// supports `subscribe`; an `unsubscribe` action is left to a future
// PR (frontend just opens a new conn for now).
type clientMsg struct {
	Action   string   `json:"action"`
	Channels []string `json:"channels"`
}

// Allowed client → server actions.
const (
	actionSubscribe = "subscribe"
)

// subscribeTo updates the per-conn channel set. Unknown channels
// are silently dropped (rather than rejecting the whole subscribe)
// so a partial-success client (`["bvol", "future-index"]`) still
// gets BVOL.
func (c *Conn) subscribeTo(channels []string) {
	c.mu.Lock()
	defer c.mu.Unlock()
	for _, ch := range channels {
		switch ch {
		case "bvol", "evol":
			c.channels[ch] = struct{}{}
		default:
			// silently ignored — see godoc above
		}
	}
}

// --- read / write goroutines --------------------------------------

// readLoop drains client → server frames. The protocol is
// effectively one-shot — clients send a single `subscribe`
// envelope and then only receive — but we keep the loop draining
// so a client that sends garbage doesn't fill kernel buffers.
func (c *Conn) readLoop(hub *Hub) {
	defer hub.Unregister(c)
	defer c.close()

	c.ws.SetReadLimit(MaxClientMsgBytes)
	// Reset the read deadline on every pong. `PingInterval +
	// PongTimeout` is the longest a healthy connection can stay
	// silent before we declare it dead.
	_ = c.ws.SetReadDeadline(time.Now().Add(PingInterval + PongTimeout))
	c.ws.SetPongHandler(func(string) error {
		_ = c.ws.SetReadDeadline(time.Now().Add(PingInterval + PongTimeout))
		return nil
	})

	for {
		_, raw, err := c.ws.ReadMessage()
		if err != nil {
			// Normal close or read deadline — exit quietly.
			return
		}
		var msg clientMsg
		if err := json.Unmarshal(raw, &msg); err != nil {
			c.tryPush(errorFrame("bad_request", "message is not valid JSON"))
			continue
		}
		switch msg.Action {
		case actionSubscribe:
			c.subscribeTo(msg.Channels)
		default:
			c.tryPush(errorFrame("bad_request", "unknown action (allowed: subscribe)"))
		}
	}
}

// writeLoop pulls fanned-out frames from the per-conn send channel
// and writes them to the WS. Also fires the keepalive ping every
// `PingInterval`.
func (c *Conn) writeLoop() {
	ticker := time.NewTicker(PingInterval)
	defer ticker.Stop()
	defer c.close()

	for {
		select {
		case <-c.closed:
			return
		case frame, ok := <-c.send:
			if !ok {
				return
			}
			_ = c.ws.SetWriteDeadline(time.Now().Add(PongTimeout))
			if err := c.ws.WriteMessage(websocket.TextMessage, frame); err != nil {
				return
			}
		case <-ticker.C:
			_ = c.ws.SetWriteDeadline(time.Now().Add(PongTimeout))
			if err := c.ws.WriteMessage(websocket.PingMessage, nil); err != nil {
				return
			}
		}
	}
}

// errorFrame builds a `{ "type":"error", "code":..., "message":... }`
// envelope. Kept inline rather than a separate handler module
// because the only producer is the read loop above.
func errorFrame(code, message string) []byte {
	b, _ := json.Marshal(map[string]any{
		"type":    "error",
		"code":    code,
		"message": message,
	})
	return b
}

// --- HTTP upgrade entry point -------------------------------------

// Handler returns an `http.HandlerFunc` that upgrades to WebSocket
// and starts the per-conn goroutines. fiber v3 has no native WS
// helper; the bridge is `middleware/adaptor.HTTPHandler(streamHandler)`
// in `cmd/api/main.go`.
//
// `limit` is the per-IP active-connection cap (PRD §6 anon: 5).
func Handler(hub *Hub, limit *IPLimiter) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		ip := clientIP(r)
		if !limit.Acquire(ip) {
			http.Error(w, "too many connections from this IP", http.StatusTooManyRequests)
			return
		}
		ws, err := upgrader.Upgrade(w, r, nil)
		if err != nil {
			limit.Release(ip)
			slog.Warn("ws upgrade failed", "error", err, "remote", r.RemoteAddr)
			return
		}
		conn := newConn(ws)
		hub.Register(conn)

		// `readLoop` returns first (via close/error/deadline); it
		// unregisters + closes. `writeLoop` then exits on the
		// `closed` channel. Release the IP slot when both end.
		var wg sync.WaitGroup
		wg.Add(2)
		go func() { defer wg.Done(); conn.readLoop(hub) }()
		go func() { defer wg.Done(); conn.writeLoop() }()
		wg.Wait()
		limit.Release(ip)
	}
}
