import type { NextConfig } from "next";

// In local dev the Go API runs on :8080. Cross-origin from :3000 would
// require CORS middleware on the API; rewriting at the Next layer keeps
// the frontend same-origin without any backend change.
const API_TARGET = process.env.API_PROXY_TARGET ?? "http://localhost:8080";

const nextConfig: NextConfig = {
  reactStrictMode: true,
  poweredByHeader: false,
  experimental: {
    typedRoutes: true,
  },
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
