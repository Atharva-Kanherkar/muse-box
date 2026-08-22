import { useMemo } from "react";
import type { RenderDoc } from "../lib/types";

interface Props {
  doc: RenderDoc | null;
  /** Interpolated position — the same value that drives the progress bar. */
  progressMs: number;
}

/**
 * Karaoke, not a document: one line at a time, keyed so each change remounts
 * and animates in while the old one is simply gone.
 *
 * The previous version rendered the whole track as a scrolling list and moved a
 * highlight through it, which is where the overlapping-text bug lived — smooth
 * scrollIntoView fighting re-renders. There is nothing to scroll now.
 */
export function Lyrics({ doc, progressMs }: Props) {
  const lyrics = doc?.lyrics ?? null;

  const { current, next } = useMemo(() => {
    if (!lyrics?.synced) return { current: -1, next: -1 };
    let current = -1;
    for (let index = 0; index < lyrics.lines.length; index += 1) {
      const line = lyrics.lines[index];
      if (line && line.at_ms <= progressMs) current = index;
      else break;
    }
    // The next line someone will actually hear, skipping instrumental gaps.
    let next = current + 1;
    while (next < lyrics.lines.length && !lyrics.lines[next]?.text.trim()) {
      next += 1;
    }
    return { current, next };
  }, [lyrics, progressMs]);

  if (!lyrics || lyrics.lines.length === 0) return null;

  // Unsynced lyrics cannot follow the song; pretending would drift visibly.
  // Shown quiet and whole instead.
  if (!lyrics.synced) {
    return (
      <aside className="karaoke" aria-label="Lyrics">
        <div className="karaoke-plain">
          {lyrics.lines.map((line, index) => (
            <p key={index}>{line.text}</p>
          ))}
        </div>
      </aside>
    );
  }

  const line = current >= 0 ? (lyrics.lines[current]?.text.trim() ?? "") : "";
  const upcoming = lyrics.lines[next]?.text.trim() ?? "";

  return (
    <aside className="karaoke" aria-label="Lyrics">
      {/* Keyed on the index: a new line replaces the old, never joins it. */}
      {line ? (
        <p className="karaoke-line" key={current}>
          {line}
        </p>
      ) : (
        // An instrumental gap: nothing sung, so nothing shown.
        <p className="karaoke-line karaoke-rest" key={`rest-${current}`} />
      )}
      {upcoming ? <p className="karaoke-next">{upcoming}</p> : null}
    </aside>
  );
}
