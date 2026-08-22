import { useEffect, useMemo, useState } from "react";
import { DevicePreview } from "./components/DevicePreview";
import { NowPlaying } from "./components/NowPlaying";
import { VoiceControl } from "./components/VoiceControl";
import {
  runStateStream,
  stateUrl,
  type ConnectionStatus,
} from "./lib/stateStream";
import {
  DEFAULT_RENDER_PARAMS,
  type RenderDoc,
  type RenderParams,
} from "./lib/types";

const STORAGE_KEY = "muse-box.connection";

interface Connection {
  baseUrl: string;
  token: string;
}

function loadConnection(): Connection {
  const fallback: Connection = {
    baseUrl: import.meta.env.VITE_API_BASE_URL ?? "http://localhost:3000",
    token: "",
  };
  try {
    const raw = window.localStorage.getItem(STORAGE_KEY);
    if (!raw) return fallback;
    const parsed = JSON.parse(raw) as Partial<Connection>;
    return {
      baseUrl: parsed.baseUrl ?? fallback.baseUrl,
      token: parsed.token ?? fallback.token,
    };
  } catch {
    return fallback;
  }
}

function statusText(status: ConnectionStatus): string {
  switch (status.kind) {
    case "idle":
      return "Not connected";
    case "connecting":
      return "Connecting…";
    case "open":
      return "Live";
    case "retrying":
      return `${status.reason} — retrying in ${Math.round(status.delayMs / 100) / 10}s`;
    case "failed":
      return status.reason;
  }
}

export default function App() {
  const [connection, setConnection] = useState<Connection>(loadConnection);
  const [draft, setDraft] = useState<Connection>(connection);
  const [params, setParams] = useState<RenderParams>(DEFAULT_RENDER_PARAMS);
  const [doc, setDoc] = useState<RenderDoc | null>(null);
  const [status, setStatus] = useState<ConnectionStatus>({ kind: "idle" });

  const configured = connection.baseUrl.trim() !== "" && connection.token !== "";

  // Re-subscribe whenever the connection or the requested frame size changes;
  // the render parameters are part of the stream URL.
  useEffect(() => {
    if (!configured) {
      setStatus({ kind: "idle" });
      return;
    }
    let url: string;
    try {
      url = stateUrl(connection.baseUrl, params);
    } catch {
      setStatus({ kind: "failed", reason: "Backend URL is not a valid URL" });
      return;
    }

    const controller = new AbortController();
    void runStateStream({
      url,
      token: connection.token,
      onDocument: setDoc,
      onStatus: setStatus,
      signal: controller.signal,
    });
    return () => controller.abort();
  }, [configured, connection.baseUrl, connection.token, params]);

  // Let the album's own colors drive the page.
  const palette = useMemo(
    () => ({
      background: doc?.palette[0] ?? "#1a1a1a",
      accent: doc?.palette[1] ?? "#e0e0e0",
    }),
    [doc?.palette],
  );

  useEffect(() => {
    const root = document.documentElement;
    root.style.setProperty("--doc-bg", palette.background);
    root.style.setProperty("--doc-accent", palette.accent);
  }, [palette]);

  function save(event: React.FormEvent) {
    event.preventDefault();
    const next: Connection = {
      baseUrl: draft.baseUrl.trim().replace(/\/+$/, ""),
      token: draft.token.trim(),
    };
    setConnection(next);
    setDoc(null);
    try {
      window.localStorage.setItem(STORAGE_KEY, JSON.stringify(next));
    } catch {
      // Private browsing: the session still works, it just will not persist.
    }
  }

  return (
    <div className="app">
      <header className="bar">
        <span className="brand">muse&#8209;box</span>
        <span className="status">
          <span className="dot" data-kind={status.kind} />
          {statusText(status)}
        </span>
      </header>

      <section className="panel">
        <h2 className="panel-title">Connection</h2>
        <form className="settings" onSubmit={save}>
          <label className="field">
            Backend URL
            <input
              type="url"
              placeholder="https://muse-box.up.railway.app"
              value={draft.baseUrl}
              onChange={(event) =>
                setDraft({ ...draft, baseUrl: event.target.value })
              }
            />
          </label>
          <label className="field">
            Device token
            <input
              type="password"
              placeholder="DEVICE_API_TOKEN"
              autoComplete="off"
              value={draft.token}
              onChange={(event) =>
                setDraft({ ...draft, token: event.target.value })
              }
            />
          </label>
          <button className="primary" type="submit">
            Connect
          </button>
        </form>
      </section>

      <div className="grid">
        <div style={{ display: "grid", gap: "var(--gap)" }}>
          <NowPlaying doc={doc} />
          <VoiceControl
            baseUrl={connection.baseUrl}
            token={connection.token}
            disabled={!configured}
          />
        </div>

        <div style={{ display: "grid", gap: "var(--gap)" }}>
          <DevicePreview doc={doc} params={params} onParamsChange={setParams} />
          <section className="panel">
            <h2 className="panel-title">Voice log</h2>
            {doc && doc.voice_log.length > 0 ? (
              <ul className="log">
                {doc.voice_log.map((entry) => (
                  <li key={`${entry.timestamp}-${entry.action}`}>
                    <span className="log-transcript">
                      “{entry.transcript}”
                    </span>
                    <span className="log-action">{entry.action}</span>
                    <span className="log-time">
                      {new Date(entry.timestamp).toLocaleTimeString()}
                    </span>
                  </li>
                ))}
              </ul>
            ) : (
              <p className="empty">No commands yet.</p>
            )}
          </section>
        </div>
      </div>

      <p className="footnote">
        render document v{doc?.version ?? 1}
        {doc?.server_ts
          ? ` · updated ${new Date(doc.server_ts).toLocaleTimeString()}`
          : ""}
      </p>
    </div>
  );
}
