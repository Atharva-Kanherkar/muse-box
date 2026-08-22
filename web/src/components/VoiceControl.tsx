import { useCallback, useEffect, useRef, useState } from "react";
import {
  MAX_RECORDING_SECONDS,
  VoiceRecorder,
  sendVoiceCommand,
} from "../lib/voice";

interface Props {
  baseUrl: string;
  token: string;
  disabled: boolean;
}

type Phase = "ready" | "recording" | "sending";

export function VoiceControl({ baseUrl, token, disabled }: Props) {
  const [phase, setPhase] = useState<Phase>("ready");
  const [elapsed, setElapsed] = useState(0);
  const [error, setError] = useState<string | null>(null);
  const recorderRef = useRef<VoiceRecorder | null>(null);
  const timerRef = useRef<number | null>(null);

  const clearTimer = useCallback(() => {
    if (timerRef.current !== null) {
      window.clearInterval(timerRef.current);
      timerRef.current = null;
    }
  }, []);

  const finish = useCallback(async () => {
    const recorder = recorderRef.current;
    if (!recorder) return;
    recorderRef.current = null;
    clearTimer();

    const recording = await recorder.stop();
    if (!recording) {
      setPhase("ready");
      setError("Nothing was recorded. Hold the button while speaking.");
      return;
    }

    setPhase("sending");
    try {
      await sendVoiceCommand(baseUrl, token, recording);
      // The resulting document arrives over the stream, so nothing to apply
      // here; the response is only useful for surfacing failures.
      setError(null);
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : "Voice command failed");
    } finally {
      setPhase("ready");
      setElapsed(0);
    }
  }, [baseUrl, clearTimer, token]);

  const begin = useCallback(async () => {
    if (disabled || phase !== "ready") return;
    setError(null);
    const recorder = new VoiceRecorder();
    try {
      await recorder.start();
    } catch (cause) {
      const message =
        cause instanceof Error ? cause.message : "Microphone unavailable";
      setError(
        message.includes("denied") || message.includes("NotAllowed")
          ? "Microphone permission denied."
          : message,
      );
      return;
    }
    recorderRef.current = recorder;
    setPhase("recording");
    setElapsed(0);
    timerRef.current = window.setInterval(() => {
      const seconds = recorder.elapsedSeconds;
      setElapsed(seconds);
      // The backend rejects anything past the cap, so send at the boundary
      // rather than letting the upload be refused.
      if (seconds >= MAX_RECORDING_SECONDS) void finish();
    }, 100);
  }, [disabled, finish, phase]);

  useEffect(() => {
    return () => {
      clearTimer();
      void recorderRef.current?.cancel();
    };
  }, [clearTimer]);

  const label =
    phase === "recording"
      ? `Recording ${elapsed.toFixed(1)}s — release to send`
      : phase === "sending"
        ? "Thinking…"
        : "Hold to talk";

  return (
    <section className="panel">
      <h2 className="panel-title">Voice</h2>
      <button
        type="button"
        className="talk"
        data-recording={phase === "recording"}
        disabled={disabled || phase === "sending"}
        onPointerDown={(event) => {
          event.preventDefault();
          void begin();
        }}
        onPointerUp={() => void finish()}
        onPointerLeave={() => {
          if (phase === "recording") void finish();
        }}
        onPointerCancel={() => void finish()}
      >
        {label}
      </button>
      <p className="hint">
        {disabled
          ? "Set the backend URL and device token first."
          : `Raw PCM16 mono, capped at ${MAX_RECORDING_SECONDS}s. Try “pause”, “next”, or “play something calm”.`}
      </p>
      {error ? <p className="error">{error}</p> : null}
    </section>
  );
}
