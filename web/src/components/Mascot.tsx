import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { RenderDoc } from "../lib/types";

/**
 * Bitka: the pixel cat who lives with the box.
 *
 * Fine pixel grid, fluffy pink silhouette, dark visor face with glowing eyes,
 * proper headphones, a terminal `>` on the chest, and an iced americano she is
 * not sharing. The body keeps its own pink; the eyes take the album accent.
 *
 * She can be picked up and carried anywhere on the screen (the spot persists),
 * and petting her — a tap or click that does not drag — gets a reaction.
 */

// One char per cell. . empty · L light pink · P pink · D deep pink ·
// K dark (visor + headphones) · W cream · C coffee · I ice.
const CAT: string[] = [
  "......P...KKKKKKKK...P........",
  ".....PLP.KKKKKKKKKK.PDP.......",
  "......LLLLLLLLLLLLLLLL........",
  ".....PPPPPPPPPPPPPPPPPP.......",
  "....PPPPPPPPPPPPPPPPPPPD......",
  "..KKPPPPKKKKKKKKKKKKPPPDKK....",
  "..KKPPPKKKKKKKKKKKKKKPPDKK....",
  "..WKPPPKKKKKKKKKKKKKKPPDKW....",
  "..KKPPPKKKKKKKKKKKKKKPPDKK....",
  "..KKPPPKKKKKKKKKKKKKKPPDKK....",
  "..KKPPPPKKKKKKKKKKKKPPPDKKP...",
  "....PPDDPPPPPPPPPPPPDDPD..P...",
  "....PPPPPPPPPPPPPPPPPPPD.PP...",
  ".....PPPPPPPPPPPPPPPPPP.WWWW..",
  ".........PPPPPPPPPP.....W..W..",
  ".....PPPPPPWPPPPPPPDPPP.WICW..",
  ".....PPPPPPPWPPPPPPD.PPPWCCW..",
  ".....LPPPPPWPPPPPPPD....WCIW..",
  "...D...DPPPPPPPPPPPD....WICW..",
  "...D..D.PPPPPPPPPPPD....WWWW..",
  "....DD...PPPD..PPPD...........",
  ".........PPPD..PPPD...........",
  ".........PPPD..PPPD...........",
  ".........PPPD..PPPD...........",
  "..............................",
  "..............................",
];

const COLORS: Record<string, string> = {
  L: "#f6d3e0",
  P: "#e8b4c8",
  D: "#c98aa6",
  K: "#14100f",
  W: "#f2ede4",
  C: "#3a2a20",
  I: "#d7ecf5",
};

/** Happy ^ ^ eyes, visor coordinates (col, row). */
const EYES_VIBING: Array<[number, number]> = [
  [9, 8], [10, 7], [11, 8],
  [16, 8], [17, 7], [18, 8],
];
/** Flat — — eyes for dozing. */
const EYES_DOZING: Array<[number, number]> = [
  [9, 8], [10, 8], [11, 8],
  [16, 8], [17, 8], [18, 8],
];
/** Two dots while thinking. */
const EYES_THINKING: Array<[number, number]> = [
  [10, 8],
  [17, 8],
];

const COMPLIMENTS = [
  "oh, THIS one.",
  "good taste tonight~",
  "your library never misses.",
  "this one? respect.",
  "you get it.",
];

const PET_LINES = ["mrrp!", "purrrr.", ":3", "careful, the coffee.", "hehe."];

function lineFor(seed: string, pool: string[]): string {
  let hash = 0;
  for (const char of seed) hash = (hash * 31 + char.charCodeAt(0)) | 0;
  return pool[Math.abs(hash) % pool.length] ?? pool[0]!;
}

const SPOT_KEY = "muse-box.bitka-spot";

interface Spot {
  x: number;
  y: number;
}

function loadSpot(): Spot | null {
  try {
    const raw = window.localStorage.getItem(SPOT_KEY);
    if (!raw) return null;
    const spot = JSON.parse(raw) as Spot;
    if (typeof spot.x !== "number" || typeof spot.y !== "number") return null;
    return spot;
  } catch {
    return null;
  }
}

function clampSpot(spot: Spot, width: number, height: number): Spot {
  return {
    x: Math.min(Math.max(spot.x, 0), Math.max(0, window.innerWidth - width)),
    y: Math.min(Math.max(spot.y, 0), Math.max(0, window.innerHeight - height)),
  };
}

export function Mascot({ doc }: { doc: RenderDoc | null }) {
  const state = doc?.state ?? "idle";
  const [petted, setPetted] = useState(false);
  const mood = petted
    ? "petted"
    : state === "playing"
      ? "vibing"
      : state === "thinking"
        ? "thinking"
        : "dozing";

  // Where she was carried to. Null means the default corner.
  const [spot, setSpot] = useState<Spot | null>(loadSpot);
  const rootRef = useRef<HTMLDivElement>(null);
  const dragRef = useRef<{
    pointer: number;
    offsetX: number;
    offsetY: number;
    moved: boolean;
  } | null>(null);

  const [bubble, setBubble] = useState<string | null>(null);
  const bubbleTimer = useRef<number | null>(null);
  const say = useCallback((text: string, forMs: number) => {
    if (bubbleTimer.current !== null) window.clearTimeout(bubbleTimer.current);
    setBubble(text);
    bubbleTimer.current = window.setTimeout(() => setBubble(null), forMs);
  }, []);

  // One compliment per track, only for music that is genuinely theirs.
  const praisedRef = useRef<string | null>(null);
  useEffect(() => {
    const trackId = doc?.track_id;
    if (!trackId || !doc?.in_library || state !== "playing") return;
    if (praisedRef.current === trackId) return;
    praisedRef.current = trackId;
    say(lineFor(trackId, COMPLIMENTS), 7000);
  }, [doc?.track_id, doc?.in_library, state, say]);

  const onPointerDown = useCallback((event: React.PointerEvent) => {
    const root = rootRef.current;
    if (!root) return;
    const box = root.getBoundingClientRect();
    dragRef.current = {
      pointer: event.pointerId,
      offsetX: event.clientX - box.left,
      offsetY: event.clientY - box.top,
      moved: false,
    };
    root.setPointerCapture(event.pointerId);
  }, []);

  const onPointerMove = useCallback((event: React.PointerEvent) => {
    const drag = dragRef.current;
    const root = rootRef.current;
    if (!drag || !root || event.pointerId !== drag.pointer) return;
    const next = clampSpot(
      { x: event.clientX - drag.offsetX, y: event.clientY - drag.offsetY },
      root.offsetWidth,
      root.offsetHeight,
    );
    // A few px of slop separates a pet from a carry.
    if (!drag.moved) {
      const box = root.getBoundingClientRect();
      if (Math.abs(next.x - box.left) < 4 && Math.abs(next.y - box.top) < 4) {
        return;
      }
      drag.moved = true;
    }
    setSpot(next);
  }, []);

  const onPointerUp = useCallback(
    (event: React.PointerEvent) => {
      const drag = dragRef.current;
      dragRef.current = null;
      if (!drag || event.pointerId !== drag.pointer) return;
      if (drag.moved) {
        // Remember where she was put down.
        setSpot((current) => {
          if (current) {
            try {
              window.localStorage.setItem(SPOT_KEY, JSON.stringify(current));
            } catch {
              // Private browsing: she forgets her spot, nothing worse.
            }
          }
          return current;
        });
      } else {
        // A touch, not a carry: she reacts.
        setPetted(true);
        say(lineFor(String(Date.now() >> 10), PET_LINES), 2200);
        window.setTimeout(() => setPetted(false), 1600);
      }
    },
    [say],
  );

  const eyes =
    mood === "vibing" || mood === "petted"
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
    <div
      ref={rootRef}
      className="mascot"
      data-mood={mood}
      style={
        spot
          ? { left: spot.x, top: spot.y, right: "auto", bottom: "auto" }
          : undefined
      }
      onPointerDown={onPointerDown}
      onPointerMove={onPointerMove}
      onPointerUp={onPointerUp}
      onPointerCancel={() => {
        dragRef.current = null;
      }}
      role="img"
      aria-label="Bitka, the muse-box cat"
    >
      {bubble ? <div className="mascot-bubble">{bubble}</div> : null}
      {mood === "petted" ? (
        <div className="mascot-heart" aria-hidden="true">
          <svg viewBox="0 0 7 6" shapeRendering="crispEdges">
            <g fill="#e8637f">
              <rect x="1" y="0" width="2" height="1" />
              <rect x="4" y="0" width="2" height="1" />
              <rect x="0" y="1" width="7" height="2" />
              <rect x="1" y="3" width="5" height="1" />
              <rect x="2" y="4" width="3" height="1" />
              <rect x="3" y="5" width="1" height="1" />
            </g>
          </svg>
        </div>
      ) : null}
      {mood === "dozing" ? (
        <div className="mascot-zzz" aria-hidden="true">
          <span>z</span>
          <span>z</span>
          <span>z</span>
        </div>
      ) : null}
      <svg viewBox="0 0 30 26" className="mascot-pix" shapeRendering="crispEdges">
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
