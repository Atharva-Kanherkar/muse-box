import { useEffect, useMemo, useRef } from "react";
import type { RenderDoc } from "../lib/types";

interface Props {
  doc: RenderDoc | null;
  /** Interpolated position, the same value that drives the progress bar. */
  progressMs: number;
}

/**
 * Lyrics beside the cover, following the track.
 *
 * No clock negotiation: each line carries an offset, and the caller's already
 * interpolated position picks the current one. Unsynced lyrics are shown as
 * plain text rather than scrolled, because inventing timings drifts visibly.
 */
export function Lyrics({ doc, progressMs }: Props) {
  const lyrics = doc?.lyrics ?? null;
  const listRef = useRef<HTMLOListElement>(null);

  const currentIndex = useMemo(() => {
    if (!lyrics?.synced) return -1;
    // Last line whose timestamp has passed.
    let found = -1;
    for (let index = 0; index < lyrics.lines.length; index += 1) {
      const line = lyrics.lines[index];
      if (line && line.at_ms <= progressMs) found = index;
      else break;
    }
    return found;
  }, [lyrics, progressMs]);

  useEffect(() => {
    if (currentIndex < 0) return;
    const list = listRef.current;
    const active = list?.children[currentIndex];
    active?.scrollIntoView({ block: "center", behavior: "smooth" });
  }, [currentIndex]);

  if (!lyrics || lyrics.lines.length === 0) return null;

  return (
    <aside className="lyrics" aria-label="Lyrics">
      <ol className="lyric-lines" ref={listRef} data-synced={lyrics.synced}>
        {lyrics.lines.map((line, index) => (
          <li
            key={`${line.at_ms}-${index}`}
            data-state={
              !lyrics.synced
                ? "plain"
                : index === currentIndex
                  ? "now"
                  : index < currentIndex
                    ? "past"
                    : "ahead"
            }
          >
            {/* An empty timed line is an instrumental gap; keep its space. */}
            {line.text || " "}
          </li>
        ))}
      </ol>
    </aside>
  );
}
