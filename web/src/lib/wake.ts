/**
 * Wake-word detection, so Muse only listens when it is spoken to.
 *
 * Energy-based voice activity cannot tell a song from a sentence, so with music
 * in the room it fired constantly. The browser has speech recognition of its
 * own, so it does both jobs here: it notices the name and hands over the whole
 * sentence as text. Nothing is sent until the name is heard.
 *
 * Sending text rather than PCM also removes the microphone contention that made
 * this unreliable — recognition and an AudioWorklet fighting over one
 * microphone is why it kept going deaf — along with resampling and the Realtime
 * API's minimum-buffer rejections.
 *
 * Recognition is Chromium and Safari only. Where it is missing the caller offers
 * a typed command rather than silently streaming the room.
 */

/** Minimal shape of the bits of the recognition API used here. */
interface RecognitionAlternative {
  transcript: string;
}
interface RecognitionResult {
  readonly length: number;
  readonly isFinal?: boolean;
  item(index: number): RecognitionAlternative;
  [index: number]: RecognitionAlternative;
}
interface RecognitionResultList {
  readonly length: number;
  item(index: number): RecognitionResult;
  [index: number]: RecognitionResult;
}
interface RecognitionEvent {
  resultIndex: number;
  results: RecognitionResultList;
}
interface RecognitionErrorEvent {
  error: string;
}
interface Recognition {
  continuous: boolean;
  interimResults: boolean;
  lang: string;
  onresult: ((event: RecognitionEvent) => void) | null;
  onerror: ((event: RecognitionErrorEvent) => void) | null;
  onend: (() => void) | null;
  start(): void;
  stop(): void;
  abort(): void;
}
type RecognitionConstructor = new () => Recognition;

/** Safari has been known to omit `isFinal`; treat a missing flag as final. */
function isFinalResult(result: RecognitionResult | undefined): boolean {
  return result?.isFinal !== false;
}

function constructor(): RecognitionConstructor | null {
  const scope = window as unknown as {
    SpeechRecognition?: RecognitionConstructor;
    webkitSpeechRecognition?: RecognitionConstructor;
  };
  return scope.SpeechRecognition ?? scope.webkitSpeechRecognition ?? null;
}

export function wakeWordSupported(): boolean {
  return constructor() !== null;
}

/**
 * Words that count as the name. Speech-to-text mangles a short name badly, so
 * the usual mishearings are accepted; the backend applies the same gate to the
 * real transcript, so a wrong wake here still cannot change playback.
 */
const NAMES = [
  "muse",
  "mews",
  "muze",
  "mooz",
  "moose",
  "moos",
  "mus",
  "muice",
  // "news" is what recognition reaches for most often, so it has to count.
  // Recall is loose on purpose: the backend still refuses to act on anything
  // that is not a music request, so a false wake costs a request, not an action.
  "news",
  "newz",
  "nous",
  "noose",
  "amuse",
  "muser",
  "myuse",
  "meuse",
];

export function containsWakeWord(text: string): boolean {
  return text
    .toLowerCase()
    .split(/[^a-z0-9]+/)
    .some((word) => NAMES.includes(word));
}

export interface WakeCallbacks {
  /** A finished utterance that named Muse: the whole command, already text. */
  onCommand: (transcript: string) => void;
  /** Latest interim text, for showing what it is hearing. */
  onHeard: (text: string) => void;
  onError: (message: string) => void;
}

/** Ignore repeat triggers inside this window, so one phrase wakes once. */
const RETRIGGER_GUARD_MS = 2_000;

export class WakeWordListener {
  private recognition: Recognition | null = null;
  private running = false;
  private muted = false;
  private lastWake = 0;
  private restartTimer: number | null = null;
  private consecutiveFailures = 0;

  constructor(private readonly callbacks: WakeCallbacks) {}

  start(): boolean {
    const Recognizer = constructor();
    if (!Recognizer || this.running) return false;
    this.running = true;
    this.spawn(Recognizer);
    return true;
  }

  /** Whether recognition is currently running. */
  get active(): boolean {
    return this.running;
  }

  private spawn(Recognizer: RecognitionConstructor): void {
    const recognition = new Recognizer();
    recognition.continuous = true;
    recognition.interimResults = true;
    recognition.lang = navigator.language || "en-US";

    recognition.onresult = (event) => {
      if (this.muted) return;
      for (let index = event.resultIndex; index < event.results.length; index += 1) {
        const result = event.results[index];
        const text = result?.[0]?.transcript?.trim() ?? "";
        if (!text) continue;
        this.callbacks.onHeard(text);
        if (!containsWakeWord(text)) continue;
        // Interim results grow as the sentence is spoken; waiting for the final
        // one means the command arrives whole rather than truncated.
        if (!isFinalResult(result)) continue;
        const now = Date.now();
        if (now - this.lastWake < RETRIGGER_GUARD_MS) continue;
        this.lastWake = now;
        this.callbacks.onCommand(text);
      }
    };

    recognition.onerror = (event) => {
      // "no-speech" and "aborted" are routine in a quiet room; only a refused
      // microphone is worth surfacing and stopping for.
      if (event.error === "not-allowed" || event.error === "service-not-allowed") {
        this.running = false;
        this.callbacks.onError(
          "Speech recognition was blocked, so the wake word cannot be heard.",
        );
      }
    };

    recognition.onend = () => {
      // Recognition ends itself after every silence gap. That is routine, not a
      // failure: counting it pushed the restart delay to seconds and left the
      // wake word deaf most of the time. Only a start that throws is a failure,
      // and a fresh instance is required because Chrome refuses to restart an
      // ended one.
      if (!this.running) return;
      const delay = this.consecutiveFailures === 0
        ? 0
        : Math.min(250 * this.consecutiveFailures, 3_000);
      this.restartTimer = window.setTimeout(() => {
        if (!this.running) return;
        this.spawn(Recognizer);
      }, delay);
    };

    this.recognition = recognition;
    try {
      recognition.start();
    } catch (cause) {
      this.running = false;
      this.callbacks.onError(
        cause instanceof Error ? cause.message : "Could not start listening",
      );
    }
  }

  /** Stop reacting without tearing down, so Muse does not hear its own voice. */
  setMuted(muted: boolean): void {
    this.muted = muted;
  }

  stop(): void {
    this.running = false;
    if (this.restartTimer !== null) {
      window.clearTimeout(this.restartTimer);
      this.restartTimer = null;
    }
    const recognition = this.recognition;
    this.recognition = null;
    if (!recognition) return;
    recognition.onresult = null;
    recognition.onerror = null;
    recognition.onend = null;
    try {
      recognition.abort();
    } catch {
      // Already stopped.
    }
  }
}
