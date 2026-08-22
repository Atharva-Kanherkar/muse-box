import { useEffect, useMemo, useRef, useState } from "react";
import type { RenderDoc } from "../lib/types";

/**
 * Bitka: the pixel cat who lives with the box.
 *
 * Drawn on a fine pixel grid — fluffy silhouette, a dark visor face with
 * glowing eyes, a terminal `>` on the chest — because this mascot was born in
 * the same 1-bit world as the device panel. The body stays its own pink; only
 * the eyes and glow take the album's accent, so it reacts to the music without
 * dissolving into it.
 *
 * States: vibing (playing — bobs in hard steps at the track's tempo), dozing
 * (idle or paused — flat eyes, drifting zzz), thinking (visor dots while Muse
 * works). Compliments appear only when `in_library` says the playing track is
 * genuinely the listener's own, once per track.
 */

// One char per cell. . empty · L light pink · P pink · D deep pink ·
// K visor · W cream · S shadow.
const CAT: string[] = [
  "......PP..........PP......",
  ".....PLLP........PLLP.....",
  "....PLLDDP......PDDLLP....",
  "....PLDDDPPPPPPPPDDDLP....",
  "...PLLDPPPPPPPPPPPPDLLP...",
  "..PLLPPPPPPPPPPPPPPPPLLP..",
  "..PLPPKKKKKKKKKKKKKKPPLP..",
  ".PLPPKKKKKKKKKKKKKKKKPPDP.",
  ".PLPKKKKKKKKKKKKKKKKKKPDP.",
  ".PLPKKKKKKKKKKKKKKKKKKPDP.",
  ".PLPKKKKKKKKKKKKKKKKKKPDP.",
  "..PPPKKKKKKKKKKKKKKKKPPD..",
  "..PLPPKKKKKKKKKKKKKKPPDD..",
  "...PLLPPPPPPPPPPPPPPDDD...",
  "....PPPPPPPPPPPPPPPPDD....",
  "......PPPPPPPPPPPPPP......",
  "....PPPPPPPPPPPPPPPPPP....",
  "...PLPPPPPPPPPPPPPPPPDP...",
  "...PLPPPWPPPPPPPPPPPPDP...",
  "...PPPPPPWPPPPPPPPPPPDP...",
  "...PPPPPWPPPPPPPPPPPPDP...",
  "....PPPPPPPPPPPPPPPPPP....",
  ".....PPPPPP....PPPPPP.....",
  ".....PLPPPP....PPPPDP.....",
  ".....PPPPPP....PPPPPP.....",
];

const COLORS: Record<string, string> = {
  L: "#f6d3e0",
  P: "#e8b4c8",
  D: "#c98aa6",
  K: "#14100f",
  W: "#f2ede4",
  S: "#a96f8c",
};

/** Happy ^ ^ eyes, visor coordinates (col, row). */
const EYES_VIBING: Array<[number, number]> = [
  [8, 9], [9, 8], [10, 9],
  [15, 9], [16, 8], [17, 9],
];
/** Flat — — eyes for dozing. */
const EYES_DOZING: Array<[number, number]> = [
  [8, 9], [9, 9], [10, 9],
  [15, 9], [16, 9], [17, 9],
];
/** Two dots while thinking. */
const EYES_THINKING: Array<[number, number]> = [
  [9, 9],
  [16, 9],
];

const COMPLIMENTS = [
  "oh, THIS one.",
  "good taste tonight~",
  "your library never misses.",
  "this one? respect.",
  "you get it.",
];

function complimentFor(trackId: string): string {
  // Deterministic per track, so the same song always earns the same line.
  let hash = 0;
  for (const char of trackId) hash = (hash * 31 + char.charCodeAt(0)) | 0;
  return COMPLIMENTS[Math.abs(hash) % COMPLIMENTS.length] ?? COMPLIMENTS[0]!;
}

export function Mascot({ doc }: { doc: RenderDoc | null }) {
  const state = doc?.state ?? "idle";
  const mood =
    state === "playing" ? "vibing" : state === "thinking" ? "thinking" : "dozing";

  // One compliment per track, and only for music that is genuinely theirs.
  const [bubble, setBubble] = useState<string | null>(null);
  const praisedRef = useRef<string | null>(null);
  useEffect(() => {
    const trackId = doc?.track_id;
    if (!trackId || !doc?.in_library || state !== "playing") return;
    if (praisedRef.current === trackId) return;
    praisedRef.current = trackId;
    setBubble(complimentFor(trackId));
    const timer = window.setTimeout(() => setBubble(null), 7000);
    return () => window.clearTimeout(timer);
  }, [doc?.track_id, doc?.in_library, state]);

  const eyes =
    mood === "vibing"
      ? EYES_VIBING
      : mood === "thinking"
        ? EYES_THINKING
        : EYES_DOZING;

  const body = useMemo(
    () =>
      CAT.flatMap((row, y) =>
        [...row].flatMap((cell, x) => {
          const fill = COLORS[cell];
          if (!fill) return [];
          return [
            <rect key={`${x}-${y}`} x={x} y={y} width={1} height={1} fill={fill} />,
          ];
        }),
      ),
    [],
  );

  return (
    <div className="mascot" data-mood={mood} aria-hidden="true">
      {bubble ? <div className="mascot-bubble">{bubble}</div> : null}
      {mood === "dozing" ? (
        <div className="mascot-zzz">
          <span>z</span>
          <span>z</span>
          <span>z</span>
        </div>
      ) : null}
      <svg
        viewBox="0 0 26 25"
        className="mascot-pix"
        shapeRendering="crispEdges"
      >
        {body}
        <g className="mascot-eyes">
          {eyes.map(([x, y]) => (
            <rect key={`${x}-${y}`} x={x} y={y} width={1} height={1} />
          ))}
        </g>
      </svg>
    </div>
  );
}
