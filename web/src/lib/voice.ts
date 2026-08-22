import type { ApiError, RenderDoc } from "./types";

/**
 * Microphone capture for `POST /voice`.
 *
 * The backend accepts only `audio/wav` and `audio/pcm` — webm/opus is refused
 * on purpose, so `MediaRecorder` is unusable here. We capture float samples
 * through an AudioWorklet, convert to PCM16, and post raw PCM with the mic's
 * own sample rate in the query string. Raw PCM accepts any positive rate, so
 * no client-side resampling is needed; the backend resamples to 24 kHz.
 */

/** The backend rejects anything longer, so stop before it does. */
export const MAX_RECORDING_SECONDS = 30;

const WORKLET_SOURCE = `
class PcmCollector extends AudioWorkletProcessor {
  process(inputs) {
    const channel = inputs[0] && inputs[0][0];
    if (channel && channel.length > 0) {
      // Copy: the render quantum buffer is reused after this returns.
      this.port.postMessage(new Float32Array(channel));
    }
    return true;
  }
}
registerProcessor("pcm-collector", PcmCollector);
`;

function floatToPcm16(chunks: Float32Array[], totalSamples: number): ArrayBuffer {
  const buffer = new ArrayBuffer(totalSamples * 2);
  const view = new DataView(buffer);
  let offset = 0;
  for (const chunk of chunks) {
    for (let index = 0; index < chunk.length; index += 1) {
      const sample = Math.max(-1, Math.min(1, chunk[index] ?? 0));
      // Asymmetric scaling matches the i16 range without clipping at +1.0.
      const value = sample < 0 ? sample * 0x8000 : sample * 0x7fff;
      view.setInt16(offset, value, true);
      offset += 2;
    }
  }
  return buffer;
}

export interface Recording {
  pcm: ArrayBuffer;
  sampleRate: number;
  durationSeconds: number;
}

/**
 * A single push-to-talk session. `stop()` resolves with the captured audio;
 * `cancel()` discards it. Either one releases the microphone.
 */
export class VoiceRecorder {
  private context: AudioContext | null = null;
  private stream: MediaStream | null = null;
  private node: AudioWorkletNode | null = null;
  private source: MediaStreamAudioSourceNode | null = null;
  private chunks: Float32Array[] = [];
  private totalSamples = 0;
  private stopped = false;

  async start(): Promise<void> {
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
    this.node.port.onmessage = (event: MessageEvent<Float32Array>) => {
      if (this.stopped) return;
      const maxSamples = context.sampleRate * MAX_RECORDING_SECONDS;
      if (this.totalSamples >= maxSamples) return;
      this.chunks.push(event.data);
      this.totalSamples += event.data.length;
    };
    this.source = context.createMediaStreamSource(this.stream);
    this.source.connect(this.node);
    // The graph only pulls if it reaches the destination, but routing the mic
    // to the speakers would howl, so terminate through a muted gain node.
    const silence = context.createGain();
    silence.gain.value = 0;
    this.node.connect(silence);
    silence.connect(context.destination);
  }

  get elapsedSeconds(): number {
    if (!this.context) return 0;
    return this.totalSamples / this.context.sampleRate;
  }

  async stop(): Promise<Recording | null> {
    const sampleRate = this.context?.sampleRate ?? 0;
    const samples = this.totalSamples;
    const chunks = this.chunks;
    await this.teardown();
    if (!sampleRate || samples === 0) return null;
    return {
      pcm: floatToPcm16(chunks, samples),
      sampleRate,
      durationSeconds: samples / sampleRate,
    };
  }

  async cancel(): Promise<void> {
    await this.teardown();
  }

  private async teardown(): Promise<void> {
    if (this.stopped) return;
    this.stopped = true;
    if (this.node) {
      this.node.port.onmessage = null;
      this.node.disconnect();
    }
    this.source?.disconnect();
    for (const track of this.stream?.getTracks() ?? []) track.stop();
    if (this.context && this.context.state !== "closed") {
      await this.context.close();
    }
    this.chunks = [];
    this.node = null;
    this.source = null;
    this.stream = null;
    this.context = null;
  }
}

/** Posts raw PCM16 to `/voice` and returns the document the backend rebuilt. */
export async function sendVoiceCommand(
  baseUrl: string,
  token: string,
  recording: Recording,
  signal?: AbortSignal,
): Promise<RenderDoc> {
  const url = new URL("/voice", baseUrl);
  url.searchParams.set("rate", String(Math.round(recording.sampleRate)));
  url.searchParams.set("bits", "16");
  url.searchParams.set("ch", "1");

  const response = await fetch(url, {
    method: "POST",
    headers: {
      Authorization: `Bearer ${token}`,
      "Content-Type": "audio/pcm",
    },
    body: recording.pcm,
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
  return (await response.json()) as RenderDoc;
}
