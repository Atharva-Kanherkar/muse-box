/**
 * Static server for the built client.
 *
 * Deliberately dependency-free: this serves a handful of hashed assets and one
 * HTML file, which is not worth a framework or a supply chain. Railway supplies
 * PORT; everything else is fixed.
 */
import { createReadStream, existsSync, statSync } from "node:fs";
import { createServer } from "node:http";
import { extname, join, normalize, resolve } from "node:path";

const root = resolve(import.meta.dirname, "dist");
const port = Number.parseInt(process.env.PORT ?? "8080", 10);

const TYPES = new Map([
  [".html", "text/html; charset=utf-8"],
  [".js", "text/javascript; charset=utf-8"],
  [".css", "text/css; charset=utf-8"],
  [".json", "application/json; charset=utf-8"],
  [".svg", "image/svg+xml"],
  [".png", "image/png"],
  [".jpg", "image/jpeg"],
  [".webp", "image/webp"],
  [".ico", "image/x-icon"],
  [".map", "application/json; charset=utf-8"],
  [".woff2", "font/woff2"],
]);

function resolveFile(urlPath) {
  // normalize collapses "..", and the prefix check rejects anything that still
  // points outside dist: a static server must never serve the filesystem.
  const candidate = resolve(join(root, normalize(decodeURIComponent(urlPath))));
  if (!candidate.startsWith(root)) return null;
  if (existsSync(candidate) && statSync(candidate).isFile()) return candidate;
  return null;
}

createServer((request, response) => {
  if (request.method !== "GET" && request.method !== "HEAD") {
    response.writeHead(405, { allow: "GET, HEAD" }).end();
    return;
  }

  const path = (request.url ?? "/").split("?")[0] ?? "/";

  // Runtime configuration: the client asks for this on boot, so the backend
  // address is a variable on this service rather than something compiled in.
  // Only the address — never the device token, which would be public here.
  if (path === "/config.json") {
    response
      .writeHead(200, {
        "content-type": "application/json; charset=utf-8",
        "cache-control": "no-store",
      })
      .end(JSON.stringify({ apiBaseUrl: process.env.API_BASE_URL ?? "" }));
    return;
  }

  if (path === "/healthz") {
    response
      .writeHead(200, { "content-type": "application/json; charset=utf-8" })
      .end('{"status":"ok"}');
    return;
  }

  // Single-page app: unknown paths fall back to the shell.
  const file = resolveFile(path) ?? join(root, "index.html");
  const type = TYPES.get(extname(file)) ?? "application/octet-stream";
  // Hashed asset names make them immutable; the shell must never be cached.
  const cache = file.endsWith("index.html")
    ? "no-cache"
    : "public, max-age=31536000, immutable";

  response.writeHead(200, { "content-type": type, "cache-control": cache });
  if (request.method === "HEAD") {
    response.end();
    return;
  }
  createReadStream(file).pipe(response);
}).listen(port, "0.0.0.0", () => {
  console.log(`muse-box web listening on 0.0.0.0:${port}`);
});
