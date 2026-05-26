package storage

import (
	"context"
	"errors"
	"fmt"
	"time"

	"github.com/redis/go-redis/v9"
)

// Redis is the API's hot-cache handle. Reads `index:{id}:latest`
// keys written by the engine's `IndexSinks` (issue #20). All writes
// to those keys originate in the engine; the API is read-only.
type Redis struct {
	Client *redis.Client
}

// OpenRedis parses the URL and warms the connection with a Ping.
func OpenRedis(ctx context.Context, url string) (*Redis, error) {
	opts, err := redis.ParseURL(url)
	if err != nil {
		return nil, fmt.Errorf("redis: parse url: %w", err)
	}
	// 1 s dial timeout matches the ClickHouse client — health checks
	// must fail fast rather than block the readiness probe.
	opts.DialTimeout = 1 * time.Second
	opts.ReadTimeout = 1 * time.Second
	opts.WriteTimeout = 1 * time.Second
	// Pool sizing: same posture as ClickHouse — small + bounded.
	opts.PoolSize = 10
	opts.MinIdleConns = 2

	cli := redis.NewClient(opts)
	pingCtx, cancel := context.WithTimeout(ctx, 1*time.Second)
	defer cancel()
	if err := cli.Ping(pingCtx).Err(); err != nil {
		_ = cli.Close()
		return nil, fmt.Errorf("redis: ping: %w", err)
	}
	return &Redis{Client: cli}, nil
}

// Ping is a lightweight liveness check for `/v1/health`. The full
// dial happens once at startup; this is just a "is the connection
// still up" probe.
func (r *Redis) Ping(ctx context.Context) error {
	return r.Client.Ping(ctx).Err()
}

// ErrNotFound is returned from the `Get*` helpers below when the key
// does not exist. The handlers in `internal/handlers` translate it
// to HTTP 404 — they never expose `redis.Nil` directly.
var ErrNotFound = errors.New("storage: key not found")

// GetLatest reads `index:{ticker}:latest`. Engine (#20) writes this
// key on every 60s tick; the value is the `IndexValue` JSON
// envelope.
func (r *Redis) GetLatest(ctx context.Context, ticker string) (string, error) {
	return r.getKey(ctx, "index:"+ticker+":latest")
}

// GetLastStrip reads `index:{ticker}:last_strip`. Engine writes this
// alongside `latest` (#23); the value is the dense-grid strip
// envelope (`{index_id, ts, near, next}`).
func (r *Redis) GetLastStrip(ctx context.Context, ticker string) (string, error) {
	return r.getKey(ctx, "index:"+ticker+":last_strip")
}

func (r *Redis) getKey(ctx context.Context, key string) (string, error) {
	v, err := r.Client.Get(ctx, key).Result()
	if err != nil {
		if errors.Is(err, redis.Nil) {
			return "", ErrNotFound
		}
		return "", err
	}
	return v, nil
}

// Close releases the underlying pool.
func (r *Redis) Close() error { return r.Client.Close() }
