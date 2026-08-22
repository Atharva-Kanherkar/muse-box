import { useCallback, useEffect, useRef, useState } from "react";
import { VoiceControl } from "./components/VoiceControl";
import { decodeArt, paintArt } from "./lib/art";
import {
  runStateStream,
  stateUrl,
  type ConnectionStatus,
} from "./lib/stateStream";
import {
  DEFAULT_RENDER_PARAMS,
  formatDuration,
  interpolatedProgressMs,
  type RenderDoc,
} from "./lib/types";
import { sendControl } from "./lib/voice";

const STORAGE_KEY = "muse-box.connection";

interface Connection {
  baseUrl: string;
  token: string;
}

function loadConnection(): Connection {
  const fallback: Connection = {
    baseUrl: import.meta.env.VITE_API_BASE_URL ?? "http://localhost:3000",
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

/**
 * Only trouble gets words. A working stream needs no narration, and labelling
 * it "connecting" every time the page loads made a healthy system look flaky.
 */
function statusText(status: ConnectionStatus): string | null {
  switch (status.kind) {
    case "open":
      return null;
    case "idle":
    case "connecting":
      return null;
    case "retrying":
      return "Reconnecting";
    case "failed":
      return status.reason;
  }
}

/** Stroke icons on a 24px grid; emoji never survive as UI. */
function Icon({ shape }: { shape: "prev" | "next" | "play" | "pause" | "gear" }) {
  const paths: Record<string, React.ReactNode> = {
    prev: (
      <>
        <path d="M19 5v14L9 12z" fill="currentColor" stroke="none" />
        <line x1="6" y1="5" x2="6" y2="19" />
      </>
    ),
    next: (
      <>
        <path d="M5 5v14l10-7z" fill="currentColor" stroke="none" />
        <line x1="18" y1="5" x2="18" y2="19" />
      </>
    ),
    play: <path d="M7 4.5v15l13-7.5z" fill="currentColor" stroke="none" />,
    pause: (
      <>
        <rect x="6" y="4.5" width="4" height="15" fill="currentColor" stroke="none" />
        <rect x="14" y="4.5" width="4" height="15" fill="currentColor" stroke="none" />
      </>
    ),
    gear: (
      <>
        <circle cx="12" cy="12" r="3" />
        <path d="M12 2v3M12 19v3M2 12h3M19 12h3M4.9 4.9l2.1 2.1M17 17l2.1 2.1M19.1 4.9L17 7M7 17l-2.1 2.1" />
      </>
    ),
  };
  return (
    <svg
      viewBox="0 0 24 24"
      width="22"
      height="22"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.8"
      strokeLinecap="round"
      aria-hidden="true"
    >
      {paths[shape]}
    </svg>
  );
}

export default function App() {
  const [connection, setConnection] = useState<Connection>(loadConnection);
  const [draft, setDraft] = useState<Connection>(connection);
  const [doc, setDoc] = useState<RenderDoc | null>(null);
  const [status, setStatus] = useState<ConnectionStatus>({ kind: "idle" });
  const [setupOpen, setSetupOpen] = useState(false);
  const [progress, setProgress] = useState(0);
  const [busy, setBusy] = useState(false);
  const [controlError, setControlError] = useState<string | null>(null);
  const idleCanvasRef = useRef<HTMLCanvasElement>(null);

  const configured = connection.baseUrl.trim() !== "" && connection.token !== "";

  useEffect(() => {
    if (!configured) {
      setStatus({ kind: "idle" });
      return;
    }
    let url: string;
    try {
      url = stateUrl(connection.baseUrl, DEFAULT_RENDER_PARAMS);
    } catch {
      setStatus({ kind: "failed", reason: "Backend URL is not valid" });
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
  }, [configured, connection.baseUrl, connection.token]);

  // The album's own palette drives the whole page.
  useEffect(() => {
    const root = document.documentElement;
    root.style.setProperty("--album-bg", doc?.palette[0] ?? "#1a1a1a");
    root.style.setProperty("--album-accent", doc?.palette[1] ?? "#e0e0e0");
  }, [doc?.palette]);

  // Progress interpolates locally between documents.
  useEffect(() => {
    setProgress(interpolatedProgressMs(doc, Date.now()));
    if (doc?.state !== "playing") return;
    const timer = window.setInterval(
      () => setProgress(interpolatedProgressMs(doc, Date.now())),
      250,
    );
    return () => window.clearInterval(timer);
  }, [doc]);

  // With nothing playing there is no sleeve, but there is the dithered clock:
  // the box's own idle face becomes the cover.
  const idleArt = !doc?.art_url && doc?.art ? doc.art : null;
  useEffect(() => {
    const canvas = idleCanvasRef.current;
    if (!canvas || !idleArt) return;
    const decoded = decodeArt(idleArt);
    if (!decoded) return;
    paintArt(canvas, decoded, {
      ink: doc?.palette[1] ?? "#e0e0e0",
      background: doc?.palette[0] ?? "#1a1a1a",
    });
  }, [idleArt, doc?.palette]);

  const control = useCallback(
    (action: "play" | "pause" | "next" | "previous") => {
      if (busy || !configured) return;
      setBusy(true);
      setControlError(null);
      void sendControl(connection.baseUrl, connection.token, action)
        .catch((cause: unknown) =>
          setControlError(
            cause instanceof Error ? cause.message : "Control failed",
          ),
        )
        .finally(() => setBusy(false));
    },
    [busy, configured, connection.baseUrl, connection.token],
  );

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
      // Private browsing: works, just does not persist.
    }
  }

  const playing = doc?.state === "playing";
  const percent =
    doc && doc.duration_ms > 0
      ? Math.min(100, (progress / doc.duration_ms) * 100)
      : 0;

  return (
    <div className="scene">
      <header className="rail">
        <span className="wordmark">muse&#8209;box</span>
        {statusText(status) ? (
          <span className="rail-status">
            <span className="beacon" data-kind={status.kind} />
            {statusText(status)}
          </span>
        ) : null}
        <button
          type="button"
          className="ghost"
          aria-label="Setup"
          onClick={() => setSetupOpen(true)}
        >
          <Icon shape="gear" />
        </button>
      </header>

      <main className="centerpiece">
        <figure className="cover" data-playing={playing}>
          {doc?.art_url ? (
            <img className="cover-art" src={doc.art_url} alt="" />
          ) : idleArt ? (
            <canvas
              ref={idleCanvasRef}
              className="cover-art cover-art-bitmap"
              aria-label="Idle clock"
            />
          ) : (
            <div className="cover-art cover-empty">
              {configured ? "quiet" : "setup"}
            </div>
          )}

          <figcaption className="veil">
            <div className="transport">
              <button
                type="button"
                className="key"
                aria-label="Previous track"
                disabled={!configured || busy}
                onClick={() => control("previous")}
              >
                <Icon shape="prev" />
              </button>
              <button
                type="button"
                className="key key-main"
                aria-label={playing ? "Pause" : "Play"}
                disabled={!configured || busy}
                onClick={() => control(playing ? "pause" : "play")}
              >
                <Icon shape={playing ? "pause" : "play"} />
              </button>
              <button
                type="button"
                className="key"
                aria-label="Next track"
                disabled={!configured || busy}
                onClick={() => control("next")}
              >
                <Icon shape="next" />
              </button>
            </div>
          </figcaption>

          <div className="needle" aria-hidden="true">
            <div className="needle-fill" style={{ width: `${percent}%` }} />
          </div>
        </figure>

        <div className="titles">
          <h1 className="title">{doc?.track ?? "Nothing playing"}</h1>
          <p className="byline">
            {doc?.artist ??
              (configured ? "say “Muse” to begin" : "add your backend to begin")}
          </p>
          {doc && doc.duration_ms > 0 ? (
            <p className="times">
              {formatDuration(progress)} · {formatDuration(doc.duration_ms)}
            </p>
          ) : null}
        </div>

        {controlError ? <p className="alert">{controlError}</p> : null}
      </main>

      <footer className="dock">
        <VoiceControl
          baseUrl={connection.baseUrl}
          token={connection.token}
          disabled={!configured}
        />
      </footer>

      {setupOpen || !configured ? (
        <div
          className="veil-screen"
          role="dialog"
          aria-label="Backend setup"
          onClick={(event) => {
            if (event.target === event.currentTarget && configured) {
              setSetupOpen(false);
            }
          }}
        >
          <form className="sheet" onSubmit={save}>
            <h2 className="sheet-head">Backend</h2>
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
            <button className="controlish" type="submit">
              Save
            </button>
          </form>
        </div>
      ) : null}
    </div>
  );
}
