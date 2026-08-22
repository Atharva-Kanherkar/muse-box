import { useEffect, useState } from "react";
import {
  formatDuration,
  interpolatedProgressMs,
  type RenderDoc,
} from "../lib/types";

const STATE_LABEL: Record<string, string> = {
  idle: "Idle",
  playing: "Playing",
  paused: "Paused",
  thinking: "Thinking",
  listening: "Listening",
};

export function NowPlaying({ doc }: { doc: RenderDoc | null }) {
  const [progress, setProgress] = useState(0);

  // Progress is interpolated locally between documents: the backend sends one
  // only on a meaningful change, so a ticking bar has to come from server_ts.
  useEffect(() => {
    setProgress(interpolatedProgressMs(doc, Date.now()));
    if (doc?.state !== "playing") return;
    const timer = window.setInterval(() => {
      setProgress(interpolatedProgressMs(doc, Date.now()));
    }, 250);
    return () => window.clearInterval(timer);
  }, [doc]);

  const percent =
    doc && doc.duration_ms > 0
      ? Math.min(100, (progress / doc.duration_ms) * 100)
      : 0;

  return (
    <section className="panel">
      <div className="now">
        {doc?.art_url ? (
          <img className="cover" src={doc.art_url} alt="" />
        ) : (
          <div className="cover cover-empty">NO ART</div>
        )}

        <div>
          <span className="state-chip" data-state={doc?.state ?? "idle"}>
            {STATE_LABEL[doc?.state ?? "idle"] ?? doc?.state}
          </span>
          <h1 className="track">{doc?.track ?? "Nothing playing"}</h1>
          <p className="artist">{doc?.artist ?? "—"}</p>
          {doc?.album ? <p className="album">{doc.album}</p> : null}

          <div className="progress">
            <div className="progress-track">
              <div className="progress-fill" style={{ width: `${percent}%` }} />
            </div>
            <div className="progress-times">
              <span>{formatDuration(progress)}</span>
              <span>{formatDuration(doc?.duration_ms ?? 0)}</span>
            </div>
          </div>
        </div>
      </div>
    </section>
  );
}
