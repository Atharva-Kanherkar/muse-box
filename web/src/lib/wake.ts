/**
 * Wake-word detection, so Muse only listens when it is spoken to.
 *
 * Energy-based voice activity cannot tell a song from a sentence, so with music
 * in the room it fired constantly. This runs the browser's own continuous
 * speech recognition purely as a trigger — it never decides anything, it only
 * answers "did someone just say Muse". Command audio still goes to the backend
 * as PCM, because that is what interprets it.
 *
 * Recognition is Chromium and Safari only. Where it is missing the caller falls
 * back to an explicit talk button rather than silently streaming the room.
 */

/** Minimal shape of the bits of the recognition API used here. */
interface RecognitionAlternative {
  transcript: string;
}
interface RecognitionResult {
  readonly length: number;
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
const NAMES = ["muse", "mews", "muze", "moose", "mus"];

export function containsWakeWord(text: string): boolean {
  return text
    .toLowerCase()
    .split(/[^a-z0-9]+/)
    .some((word) => NAMES.includes(word));
}

export interface WakeCallbacks {
  onWake: () => void;
  /** Latest heard text, for showing what it thought it heard. */
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

  private spawn(Recognizer: RecognitionConstructor): void {
    const recognition = new Recognizer();
    recognition.continuous = true;
    recognition.interimResults = true;
    recognition.lang = navigator.language || "en-US";

    recognition.onresult = (event) => {
      if (this.muted) return;
      for (let index = event.resultIndex; index < event.results.length; index += 1) {
        const text = event.results[index]?.[0]?.transcript ?? "";
        if (!text.trim()) continue;
        this.callbacks.onHeard(text.trim());
        if (!containsWakeWord(text)) continue;
        const now = Date.now();
        if (now - this.lastWake < RETRIGGER_GUARD_MS) continue;
        this.lastWake = now;
        this.callbacks.onWake();
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
      // Recognition stops itself regularly; keeping it alive is the caller's job.
      if (!this.running) return;
      this.consecutiveFailures += 1;
      const delay = Math.min(200 * this.consecutiveFailures, 4_000);
      this.restartTimer = window.setTimeout(() => {
        if (!this.running) return;
        try {
          recognition.start();
          this.consecutiveFailures = 0;
        } catch {
          // Already starting; the next onend will retry.
        }
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
