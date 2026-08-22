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
    // Local convenience only: put it in .env.local (gitignored) so you are not
    // pasting the device token on every hard reload.
    token: import.meta.env.VITE_DEVICE_TOKEN ?? "",
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
  // Setup is scaffolding, not the product: once it works, get it out of the way.
  const [setupOpen, setSetupOpen] = useState(false);

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
    root.style.setProperty("--album-bg", palette.background);
    root.style.setProperty("--album-accent", palette.accent);
  }, [palette]);

  function save(event: React.FormEvent) {
    event.preventDefault();
    const next: Connection = {
      baseUrl: draft.baseUrl.trim().replace(/\/+$/, ""),
      token: draft.token.trim(),
    };
    setConnection(next);
    setDoc(null);
    setSetupOpen(false);
    try {
      window.localStorage.setItem(STORAGE_KEY, JSON.stringify(next));
    } catch {
      // Private browsing: the session still works, it just will not persist.
    }
  }

  const setupVisible = setupOpen || !configured;

  return (
    <div className="shell">
      <header className="masthead">
        <h1 className="wordmark">muse&#8209;box</h1>
        <span className="masthead-rule" />
        <span className="status-line">
          <span className="beacon" data-kind={status.kind} />
          {statusText(status)}
        </span>
        {configured ? (
          <button
            type="button"
            className="link-button"
            onClick={() => setSetupOpen((open) => !open)}
          >
            {setupOpen ? "Hide setup" : "Setup"}
          </button>
        ) : null}
      </header>

      <div className="stage">
        <div className="column">
          <DevicePreview doc={doc} params={params} onParamsChange={setParams} />
        </div>

        <div className="column">
          <NowPlaying doc={doc} />
          <VoiceControl
            baseUrl={connection.baseUrl}
            token={connection.token}
            disabled={!configured}
          />

          <section className="block">
            <h2 className="block-head">Heard</h2>
            {doc && doc.voice_log.length > 0 ? (
              <ul className="log">
                {doc.voice_log.map((entry) => (
                  <li key={`${entry.timestamp}-${entry.action}`}>
                    <span className="log-when">
                      {new Date(entry.timestamp).toLocaleTimeString([], {
                        hour: "2-digit",
                        minute: "2-digit",
                      })}
                    </span>
                    <div>
                      <p className="log-said">
                        {entry.transcript.trim() || "(nothing intelligible)"}
                      </p>
                      <span className="log-did">{entry.action}</span>
                    </div>
                  </li>
                ))}
              </ul>
            ) : (
              <p className="empty-note">Nothing said yet.</p>
            )}
          </section>

          {setupVisible ? (
            <section className="block">
              <h2 className="block-head">Backend</h2>
              <form className="setup" onSubmit={save}>
                <label className="field">
                  Address
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
                <button className="control" type="submit">
                  Connect
                </button>
              </form>
            </section>
          ) : null}
        </div>
      </div>

      <p className="colophon">
        render document v{doc?.version ?? 1}
        {doc?.server_ts
          ? ` · ${new Date(doc.server_ts).toLocaleTimeString()}`
          : ""}
      </p>
    </div>
  );
}
