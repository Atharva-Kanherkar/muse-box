import type { RenderDoc } from "./types";

/**
 * SSE client for `GET /state`, built on fetch rather than EventSource.
 *
 * EventSource cannot set request headers, and `/state` is bearer-protected, so
 * the browser's built-in SSE client cannot authenticate at all. Reading the
 * response body as a stream is the only way to send `Authorization`.
 *
 * The stream is also deliberately silent between meaningful changes, so idle
 * connections get reaped by proxies with no error the server knows about.
 * Reconnecting is cheap and lossless: the first event after connect is always a
 * complete document.
 */

export type ConnectionStatus =
  | { kind: "idle" }
  | { kind: "connecting"; attempt: number }
  | { kind: "open" }
  | { kind: "retrying"; attempt: number; delayMs: number; reason: string }
  | { kind: "failed"; reason: string };

export interface StateStreamOptions {
  url: string;
  token: string;
  onDocument: (doc: RenderDoc) => void;
  onStatus: (status: ConnectionStatus) => void;
  signal: AbortSignal;
}

const BASE_RETRY_MS = 500;
const MAX_RETRY_MS = 15_000;

function retryDelay(attempt: number): number {
  const exponential = BASE_RETRY_MS * 2 ** Math.min(attempt, 5);
  // Jitter keeps a rebooting backend from being hit by every client at once.
  return Math.min(exponential, MAX_RETRY_MS) * (0.75 + Math.random() * 0.5);
}

function sleep(ms: number, signal: AbortSignal): Promise<void> {
  return new Promise((resolve) => {
    const timer = setTimeout(resolve, ms);
    signal.addEventListener(
      "abort",
      () => {
        clearTimeout(timer);
        resolve();
      },
      { once: true },
    );
  });
}

/**
 * Parses one SSE frame. Only `data:` matters here; `event:` is always "state"
 * and `id:` is a per-event uuid the backend does not ask us to echo back.
 */
function parseFrame(frame: string): RenderDoc | null {
  const data = frame
    .split("\n")
    .filter((line) => line.startsWith("data:"))
    .map((line) => line.slice(5).trimStart())
    .join("\n");
  if (!data) return null;
  try {
    return JSON.parse(data) as RenderDoc;
  } catch {
    return null;
  }
}

async function readStream(
  response: Response,
  onDocument: (doc: RenderDoc) => void,
): Promise<void> {
  const body = response.body;
  if (!body) throw new Error("response has no readable body");
  const reader = body.pipeThrough(new TextDecoderStream()).getReader();
  // Frames are separated by a blank line. Tolerate CRLF, since a proxy may
  // rewrite line endings on the way through.
  const separator = /\r?\n\r?\n/;
  let buffer = "";
  try {
    for (;;) {
      const { done, value } = await reader.read();
      if (done) return;
      buffer += value;
      for (
        let match = separator.exec(buffer);
        match !== null;
        match = separator.exec(buffer)
      ) {
        const frame = buffer.slice(0, match.index);
        buffer = buffer.slice(match.index + match[0].length);
        const doc = parseFrame(frame);
        if (doc) onDocument(doc);
      }
    }
  } finally {
    reader.releaseLock();
  }
}

/**
 * Connects and keeps reconnecting until `signal` aborts. Resolves only when
 * aborted, or when the server rejects us in a way retrying cannot fix.
 */
export async function runStateStream({
  url,
  token,
  onDocument,
  onStatus,
  signal,
}: StateStreamOptions): Promise<void> {
  let attempt = 0;

  while (!signal.aborted) {
    onStatus({ kind: "connecting", attempt });
    try {
      const response = await fetch(url, {
        headers: { Authorization: `Bearer ${token}`, Accept: "text/event-stream" },
        signal,
        cache: "no-store",
      });

      if (response.status === 401) {
        onStatus({ kind: "failed", reason: "Device token rejected" });
        return;
      }
      if (!response.ok) {
        let detail = `Server returned ${response.status}`;
        try {
          const body = (await response.json()) as { error?: string };
          if (body.error) detail = body.error;
        } catch {
          // Non-JSON error body; the status is enough.
        }
        // 4xx other than 401 means our request shape is wrong, so retrying it
        // unchanged would just loop.
        if (response.status < 500) {
          onStatus({ kind: "failed", reason: detail });
          return;
        }
        throw new Error(detail);
      }

      attempt = 0;
      onStatus({ kind: "open" });
      await readStream(response, onDocument);
      if (signal.aborted) return;
      throw new Error("Stream closed by server");
    } catch (error) {
      if (signal.aborted) return;
      attempt += 1;
      const reason = error instanceof Error ? error.message : "Connection lost";
      const delayMs = retryDelay(attempt);
      onStatus({ kind: "retrying", attempt, delayMs, reason });
      await sleep(delayMs, signal);
    }
  }
}

export function stateUrl(
  baseUrl: string,
  params: { w: number; h: number; dither: string },
): string {
  const url = new URL("/state", baseUrl);
  url.searchParams.set("w", String(params.w));
  url.searchParams.set("h", String(params.h));
  url.searchParams.set("dither", params.dither);
  return url.toString();
}
