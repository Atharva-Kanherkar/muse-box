/**
 * Mirror of the backend's render document (`src/render.rs`).
 *
 * `version` is 1. If the backend ever bumps it, this file is the first thing
 * that needs to change.
 */
export const RENDER_DOCUMENT_VERSION = 1;

export type PlaybackState =
  | "idle"
  | "playing"
  | "paused"
  | "thinking"
  | "listening";

export type DitherMode = "bayer" | "atkinson";

/**
 * Packed 1-bit artwork. `bits` is base64 of row-major data, MSB first within
 * each byte, every row padded to a whole byte, 1 = ink. Not an image file.
 */
export interface Art {
  w: number;
  h: number;
  dither: DitherMode;
  bits: string;
}

export interface VoiceLogEntry {
  transcript: string;
  action: string;
  timestamp: string;
}

export interface LyricLine {
  at_ms: number;
  text: string;
}

/** Absent when the track has none; `synced` false means show, do not follow. */
export interface Lyrics {
  synced: boolean;
  lines: LyricLine[];
}

export interface RenderDoc {
  version: number;
  state: PlaybackState;
  /** Build time. Progress is interpolated from this, never from local guesses. */
  server_ts: string | null;
  track_id: string | null;
  track: string | null;
  artist: string | null;
  album: string | null;
  /** Device frame. The web UI shows it as a preview and uses art_url instead. */
  art: Art | null;
  art_url: string | null;
  /** Exactly two entries: [dominant background, clamped accent]. */
  palette: string[];
  progress_ms: number;
  duration_ms: number;
  voice_log: VoiceLogEntry[];
  lyrics?: Lyrics | null;
  /** Track tempo, when Spotify will still say. Paces the ambient pulse. */
  tempo_bpm?: number | null;
  /** 0..1, scaling how hard the ambience moves. */
  energy?: number | null;
  /** True when the playing track is in the listener's own library. */
  in_library?: boolean;
}

/** Per-device render parameters accepted by `GET /state`. */
export interface RenderParams {
  w: number;
  h: number;
  dither: DitherMode;
}

export const DEFAULT_RENDER_PARAMS: RenderParams = {
  w: 400,
  h: 400,
  dither: "bayer",
};

export const MIN_RENDER_DIMENSION = 16;
export const MAX_RENDER_DIMENSION = 1024;

/** The backend's uniform error body: `{ "error": "..." }`. */
export interface ApiError {
  error: string;
}

export function isPlaying(doc: RenderDoc | null): boolean {
  return doc?.state === "playing";
}

/**
 * Progress at `now`, interpolated the way the backend documents it:
 * `progress_ms + (now - server_ts)` while playing, clamped to the track.
 */
export function interpolatedProgressMs(
  doc: RenderDoc | null,
  now: number,
): number {
  if (!doc) return 0;
  if (doc.state !== "playing" || !doc.server_ts) return doc.progress_ms;
  const elapsed = now - Date.parse(doc.server_ts);
  if (!Number.isFinite(elapsed)) return doc.progress_ms;
  return Math.min(doc.progress_ms + Math.max(elapsed, 0), doc.duration_ms);
}

export function formatDuration(milliseconds: number): string {
  if (!Number.isFinite(milliseconds) || milliseconds < 0) return "0:00";
  const total = Math.floor(milliseconds / 1000);
  const minutes = Math.floor(total / 60);
  const seconds = total % 60;
  return `${minutes}:${seconds.toString().padStart(2, "0")}`;
}
