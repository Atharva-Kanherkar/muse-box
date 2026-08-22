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

  // The backend sends a document only on a meaningful change, so a moving
  // progress bar has to be interpolated locally from server_ts.
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
  const state = doc?.state ?? "idle";

  return (
    <section className="block">
      <div className="marquee">
        {doc?.art_url ? (
          <img className="sleeve" src={doc.art_url} alt="" />
        ) : (
          <div className="sleeve sleeve-empty">No sleeve</div>
        )}

        <div>
          <span className="state-tag" data-state={state}>
            {STATE_LABEL[state] ?? state}
          </span>
          <h2 className="title">{doc?.track ?? "Nothing playing"}</h2>
          <p className="byline">
            {doc?.artist ?? "Say something to Muse"}
            {doc?.album ? (
              <span className="byline-album">{doc.album}</span>
            ) : null}
          </p>
        </div>
      </div>

      <div className="timeline">
        <div className="timeline-rail">
          <div className="timeline-fill" style={{ width: `${percent}%` }} />
        </div>
        <div className="timeline-times">
          <span>{formatDuration(progress)}</span>
          <span>{formatDuration(doc?.duration_ms ?? 0)}</span>
        </div>
      </div>
    </section>
  );
}
