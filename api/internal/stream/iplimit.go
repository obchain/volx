package stream

import (
	"net"
	"net/http"
	"sync"
)

// IPLimiter caps per-IP concurrent WebSocket connections. PRD §6
// pins the anon limit at 5; a paid-tier auth check (#M3) will route
// API-key requests through a different limiter and skip this one.
//
// Implementation is a sync.Mutex-guarded `map[ip]int`. The map
// grows with the active-IP count, not the cumulative connection
// count — `Release` decrements and deletes at zero.
type IPLimiter struct {
	max   int
	mu    sync.Mutex
	conns map[string]int
}

// NewIPLimiter constructs a limiter with the given per-IP cap.
// `max <= 0` disables the limit (used by tests).
func NewIPLimiter(max int) *IPLimiter {
	return &IPLimiter{max: max, conns: make(map[string]int)}
}

// Acquire reserves one connection slot for the IP. Returns false
// when the IP is at cap.
func (l *IPLimiter) Acquire(ip string) bool {
	if l.max <= 0 {
		return true
	}
	l.mu.Lock()
	defer l.mu.Unlock()
	if l.conns[ip] >= l.max {
		return false
	}
	l.conns[ip]++
	return true
}

// Release returns one slot. Safe to call on an IP that was never
// `Acquire`d (no-op, useful in error-recovery paths in the WS
// handler).
func (l *IPLimiter) Release(ip string) {
	if l.max <= 0 {
		return
	}
	l.mu.Lock()
	defer l.mu.Unlock()
	if c, ok := l.conns[ip]; ok {
		if c <= 1 {
			delete(l.conns, ip)
		} else {
			l.conns[ip] = c - 1
		}
	}
}

// clientIP extracts the request's source IP. `X-Forwarded-For` takes
// precedence so reverse-proxy deployments (Caddy, Cloudflare per
// PRD §13) see the real client; falls back to `RemoteAddr`.
//
// Behind a trusted proxy this is correct; without one a hostile
// client can forge X-Forwarded-For to bypass the limit. The PRD
// localhost-bind posture + Caddy front means the API never serves
// untrusted X-Forwarded-For directly.
func clientIP(r *http.Request) string {
	if xff := r.Header.Get("X-Forwarded-For"); xff != "" {
		// The leftmost entry is the original client; intermediate
		// proxies append.
		for i := 0; i < len(xff); i++ {
			if xff[i] == ',' {
				return trimSpace(xff[:i])
			}
		}
		return trimSpace(xff)
	}
	host, _, err := net.SplitHostPort(r.RemoteAddr)
	if err != nil {
		return r.RemoteAddr
	}
	return host
}

// trimSpace is a tiny stdlib-free trim so this file does not depend
// on `strings`. Trims ASCII space + tab on both ends.
func trimSpace(s string) string {
	start := 0
	for start < len(s) && (s[start] == ' ' || s[start] == '\t') {
		start++
	}
	end := len(s)
	for end > start && (s[end-1] == ' ' || s[end-1] == '\t') {
		end--
	}
	return s[start:end]
}
