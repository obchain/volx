import type { NextConfig } from "next";

// In local dev the Go API publishes on host 127.0.0.1:8090 (per the
// docker compose port mapping in docker/docker-compose.yml). Host port
// 8090 — not 8080 — because 8080 is a heavily-trafficked default on
// developer machines.
//
// 127.0.0.1 (not "localhost") is intentional: Node's fetch resolves
// "localhost" to ::1 (IPv6) first on macOS, and Docker's loopback bind
// is IPv4-only — every request would pay an IPv6 ECONNREFUSED + IPv4
// retry. Pinning IPv4 avoids the round trip.
//
// Cross-origin from :3000 would require CORS middleware on the API;
// rewriting at the Next layer keeps the frontend same-origin without
// any backend change.
const API_TARGET = process.env.API_PROXY_TARGET ?? "http://127.0.0.1:8090";

const nextConfig: NextConfig = {
  reactStrictMode: true,
  poweredByHeader: false,
  // Moved out of `experimental` in Next 15.5.
  typedRoutes: true,
  async rewrites() {
    return [
      {
        source: "/v1/:path*",
        destination: `${API_TARGET}/v1/:path*`,
      },
    ];
  },
};

export default nextConfig;
