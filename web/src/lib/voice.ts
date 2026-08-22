import type { ApiError, RenderDoc } from "./types";

/**
 * Continuous microphone listening for `POST /voice`.
 *
 * There is no push-to-talk: the listener runs until stopped, segments speech
 * with energy-based voice-activity detection, and posts each utterance. Muse
 * decides whether an utterance was aimed at it — speech that was not resolves
 * to the no-op `now_playing` tool.
 *
 * Two format constraints come from the backend: it accepts only `audio/wav` and
 * `audio/pcm` (webm/opus is refused on purpose, so `MediaRecorder` is unusable),
 * and raw PCM accepts any positive sample rate, so we send the microphone's own
 * rate and let the backend resample to 24 kHz.
 */

/** The backend rejects longer audio, so cut the utterance before it does. */
export const MAX_UTTERANCE_SECONDS = 30;
/**
 * OpenAI's Realtime API refuses a buffer under 100 ms of audio. Sending a
 * shorter clip produces "buffer too small" rather than anything useful, so
 * short blips are dropped locally.
 */
export const MIN_UTTERANCE_SECONDS = 0.45;

/** Silence needed to call an utterance finished. */
const TRAILING_SILENCE_SECONDS = 0.8;
/** Speech has to exceed the running noise floor by this factor. */
const SPEECH_OVER_NOISE = 2.5;
/** Absolute floor, so a silent room cannot trigger on its own hiss. */
const MIN_SPEECH_RMS = 0.012;
/**
 * Audio kept before the wake word fires. Recognition reports the name a beat
 * after it was said, and people say "Muse, play something calm" in one breath,
 * so the command is already spoken by then. Without this pre-roll it is lost.
 */
const PREROLL_SECONDS = 3;
/** Give up if a wake was not followed by speech: a false trigger, so no send. */
const ARMED_PATIENCE_SECONDS = 2.5;

const WORKLET_SOURCE = `
class PcmCollector extends AudioWorkletProcessor {
  constructor() {
    super();
    this.buffer = new Float32Array(1024);
    this.filled = 0;
  }
  process(inputs) {
    const channel = inputs[0] && inputs[0][0];
    if (!channel) return true;
    for (let i = 0; i < channel.length; i += 1) {
      this.buffer[this.filled++] = channel[i];
      if (this.filled === this.buffer.length) {
        // Batch ~21ms at 48kHz instead of posting every 128-sample quantum.
        this.port.postMessage(this.buffer.slice(0));
        this.filled = 0;
      }
    }
    return true;
  }
}
registerProcessor("pcm-collector", PcmCollector);
`;

function rootMeanSquare(frame: Float32Array): number {
  let sum = 0;
  for (let index = 0; index < frame.length; index += 1) {
    const sample = frame[index] ?? 0;
    sum += sample * sample;
  }
  return Math.sqrt(sum / Math.max(frame.length, 1));
}

function floatToPcm16(chunks: Float32Array[], totalSamples: number): ArrayBuffer {
  const buffer = new ArrayBuffer(totalSamples * 2);
  const view = new DataView(buffer);
  let offset = 0;
  for (const chunk of chunks) {
    for (let index = 0; index < chunk.length; index += 1) {
      const sample = Math.max(-1, Math.min(1, chunk[index] ?? 0));
      // Asymmetric scaling covers the i16 range without clipping at +1.0.
      view.setInt16(offset, sample < 0 ? sample * 0x8000 : sample * 0x7fff, true);
      offset += 2;
    }
  }
  return buffer;
}

/** Spoken reply from Muse: base64 PCM16 mono at `rate`. */
export interface Speech {
  format: string;
  rate: number;
  audio: string;
}

export interface Utterance {
  pcm: ArrayBuffer;
  sampleRate: number;
  durationSeconds: number;
}

export type ListenerPhase = "stopped" | "listening" | "speaking";

export interface ListenerCallbacks {
  onUtterance: (utterance: Utterance) => void;
  onPhase: (phase: ListenerPhase) => void;
  /** Normalised 0..1 level, for the meter. */
  onLevel: (level: number) => void;
  onError: (message: string) => void;
}

export class VoiceListener {
  private context: AudioContext | null = null;
  private stream: MediaStream | null = null;
  private node: AudioWorkletNode | null = null;
  private source: MediaStreamAudioSourceNode | null = null;

  private speech: Float32Array[] = [];
  private speechSamples = 0;
  private silenceSamples = 0;
  private noiseFloor = MIN_SPEECH_RMS;
  private running = false;
  private muted = false;

  /** Rolling window of recent audio, kept whether or not anything is armed. */
  private preroll: Float32Array[] = [];
  private prerollSamples = 0;
  /** Only true between a wake word and the end of the command that follows. */
  private armed = false;
  private armedSamples = 0;
  private heardSpeech = false;

  constructor(private readonly callbacks: ListenerCallbacks) {}

  async start(): Promise<void> {
    if (this.running) return;
    this.stream = await navigator.mediaDevices.getUserMedia({
      audio: {
        channelCount: 1,
        echoCancellation: true,
        noiseSuppression: true,
        autoGainControl: true,
      },
    });
    const context = new AudioContext();
    this.context = context;
    const blob = new Blob([WORKLET_SOURCE], { type: "application/javascript" });
    const moduleUrl = URL.createObjectURL(blob);
    try {
      await context.audioWorklet.addModule(moduleUrl);
    } finally {
      URL.revokeObjectURL(moduleUrl);
    }

    this.node = new AudioWorkletNode(context, "pcm-collector");
    this.node.port.onmessage = (event: MessageEvent<Float32Array>) =>
      this.consume(event.data, context.sampleRate);
    this.source = context.createMediaStreamSource(this.stream);
    this.source.connect(this.node);
    // The graph only pulls if it reaches the destination, but routing the mic
    // to the speakers would howl, so terminate through a muted gain node.
    const silence = context.createGain();
    silence.gain.value = 0;
    this.node.connect(silence);
    silence.connect(context.destination);

    this.running = true;
    this.callbacks.onPhase("listening");
  }

  /** Ignore input without tearing the graph down, so Muse cannot hear itself. */
  setMuted(muted: boolean): void {
    this.muted = muted;
    if (muted) this.disarm();
  }

  /**
   * Start capturing a command. Called when the wake word fires; the pre-roll is
   * folded in so the words spoken alongside the name are not lost.
   */
  arm(): void {
    if (!this.running || this.muted || this.armed) return;
    this.armed = true;
    this.armedSamples = 0;
    this.heardSpeech = false;
    this.speech = [...this.preroll];
    this.speechSamples = this.prerollSamples;
    this.silenceSamples = 0;
    this.callbacks.onPhase("speaking");
  }

  private disarm(): void {
    if (!this.armed) return;
    this.armed = false;
    this.speech = [];
    this.speechSamples = 0;
    this.silenceSamples = 0;
    this.callbacks.onPhase("listening");
  }

  private consume(frame: Float32Array, sampleRate: number): void {
    if (!this.running || this.muted) return;
    const level = rootMeanSquare(frame);
    this.callbacks.onLevel(Math.min(1, level * 12));

    const threshold = Math.max(this.noiseFloor * SPEECH_OVER_NOISE, MIN_SPEECH_RMS);
    const loud = level > threshold;

    // Always keep the rolling window, so a wake word can look backwards.
    this.preroll.push(frame);
    this.prerollSamples += frame.length;
    while (this.prerollSamples > PREROLL_SECONDS * sampleRate) {
      const dropped = this.preroll.shift();
      if (!dropped) break;
      this.prerollSamples -= dropped.length;
    }

    if (!this.armed) {
      // Nothing is being captured, so this is the room: music, conversation,
      // silence. Learn the noise floor from it and send nothing.
      this.noiseFloor = this.noiseFloor * 0.95 + level * 0.05;
      return;
    }

    this.speech.push(frame);
    this.speechSamples += frame.length;
    this.armedSamples += frame.length;
    if (loud) {
      this.heardSpeech = true;
      this.silenceSamples = 0;
    } else {
      this.silenceSamples += frame.length;
    }

    // A wake word with nothing after it was a mishearing; drop it unsent.
    if (
      !this.heardSpeech &&
      this.armedSamples / sampleRate >= ARMED_PATIENCE_SECONDS
    ) {
      this.disarm();
      return;
    }

    const trailing =
      this.heardSpeech &&
      this.silenceSamples / sampleRate >= TRAILING_SILENCE_SECONDS;
    const tooLong = this.speechSamples / sampleRate >= MAX_UTTERANCE_SECONDS;
    if (trailing || tooLong) this.finishUtterance(sampleRate);
  }

  private finishUtterance(sampleRate: number): void {
    const chunks = this.speech;
    const samples = this.speechSamples;
    this.armed = false;
    this.speech = [];
    this.speechSamples = 0;
    this.silenceSamples = 0;
    this.callbacks.onPhase("listening");

    const duration = samples / sampleRate;
    // Anything this short is a door closing or a cough, and the Realtime API
    // would reject it as too small anyway.
    if (duration < MIN_UTTERANCE_SECONDS) return;
    this.callbacks.onUtterance({
      pcm: floatToPcm16(chunks, samples),
      sampleRate,
      durationSeconds: duration,
    });
  }

  async stop(): Promise<void> {
    if (!this.running) return;
    this.running = false;
    this.armed = false;
    if (this.node) {
      this.node.port.onmessage = null;
      this.node.disconnect();
    }
    this.source?.disconnect();
    for (const track of this.stream?.getTracks() ?? []) track.stop();
    if (this.context && this.context.state !== "closed") {
      await this.context.close();
    }
    this.speech = [];
    this.preroll = [];
    this.prerollSamples = 0;
    this.node = null;
    this.source = null;
    this.stream = null;
    this.context = null;
    this.callbacks.onPhase("stopped");
    this.callbacks.onLevel(0);
  }
}

/**
 * Plays a spoken reply and resolves when it finishes.
 *
 * The music itself plays on a Spotify device rather than in this tab, so there
 * is nothing to duck; the only conflict is the microphone hearing Muse, which
 * the caller avoids by pausing the listener until this resolves.
 */
export async function playSpeech(speech: Speech): Promise<void> {
  if (speech.format !== "pcm16" || !speech.audio) return;
  const binary = atob(speech.audio);
  const samples = Math.floor(binary.length / 2);
  if (samples === 0) return;

  const context = new AudioContext();
  try {
    const buffer = context.createBuffer(1, samples, speech.rate);
    const channel = buffer.getChannelData(0);
    for (let index = 0; index < samples; index += 1) {
      const low = binary.charCodeAt(index * 2);
      const high = binary.charCodeAt(index * 2 + 1);
      // Little-endian signed 16-bit into the -1..1 float range.
      const value = (high << 8) | low;
      const signed = value >= 0x8000 ? value - 0x10000 : value;
      channel[index] = signed / 0x8000;
    }
    const source = context.createBufferSource();
    source.buffer = buffer;
    source.connect(context.destination);
    await new Promise<void>((resolve) => {
      source.onended = () => resolve();
      source.start();
    });
  } finally {
    await context.close();
  }
}

/**
 * Sends a command the browser already transcribed.
 *
 * The audio path below stays for hardware, which has no speech recognition.
 */
export async function sendCommand(
  transcript: string,
): Promise<RenderDoc & { speech?: Speech }> {
  const response = await fetch("/command", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    credentials: "same-origin",
    body: JSON.stringify({ transcript }),
  });
  if (!response.ok) {
    let message = `Muse could not act on that (${response.status})`;
    try {
      const body = (await response.json()) as ApiError;
      if (body.error) message = body.error;
    } catch {
      // Keep the status-based message.
    }
    throw new Error(message);
  }
  return (await response.json()) as RenderDoc & { speech?: Speech };
}

/** Posts one utterance as raw PCM16 and returns the rebuilt document. */
export async function sendVoiceCommand(
  utterance: Utterance,
  signal?: AbortSignal,
): Promise<RenderDoc & { speech?: Speech }> {
  const query = new URLSearchParams({
    rate: String(Math.round(utterance.sampleRate)),
    bits: "16",
    ch: "1",
  });

  const response = await fetch(`/voice?${query.toString()}`, {
    method: "POST",
    headers: { "Content-Type": "audio/pcm" },
    credentials: "same-origin",
    body: utterance.pcm,
    ...(signal ? { signal } : {}),
  });

  if (!response.ok) {
    let message = `Voice command failed (${response.status})`;
    try {
      const body = (await response.json()) as ApiError;
      if (body.error) message = body.error;
    } catch {
      // Keep the status-based message.
    }
    throw new Error(message);
  }
  return (await response.json()) as RenderDoc & { speech?: Speech };
}

/** One transport action from the on-screen player. The updated document
 * arrives over the stream, so the response body is only read for errors. */
export async function sendControl(
  action: "play" | "pause" | "next" | "previous",
): Promise<void> {
  const response = await fetch("/control", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    credentials: "same-origin",
    body: JSON.stringify({ action }),
  });
  if (!response.ok) {
    let message = `Control failed (${response.status})`;
    try {
      const body = (await response.json()) as ApiError;
      if (body.error) message = body.error;
    } catch {
      // Keep the status-based message.
    }
    throw new Error(message);
  }
}
