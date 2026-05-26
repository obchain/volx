package storage

import (
	"context"
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

// Close releases the underlying pool.
func (r *Redis) Close() error { return r.Client.Close() }
