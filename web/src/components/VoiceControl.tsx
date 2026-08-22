import { useCallback, useEffect, useRef, useState } from "react";
import {
  MIN_UTTERANCE_SECONDS,
  VoiceListener,
  sendVoiceCommand,
  type ListenerPhase,
  type Utterance,
} from "../lib/voice";

interface Props {
  baseUrl: string;
  token: string;
  disabled: boolean;
}

/**
 * Always-on listening. One click grants microphone access — browsers will not
 * open a microphone without a gesture — and after that Muse listens
 * continuously and decides for itself what was meant for it.
 */
export function VoiceControl({ baseUrl, token, disabled }: Props) {
  const [phase, setPhase] = useState<ListenerPhase>("stopped");
  const [level, setLevel] = useState(0);
  const [sending, setSending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [lastHeard, setLastHeard] = useState<string | null>(null);

  const listenerRef = useRef<VoiceListener | null>(null);
  // Read inside the utterance handler, which is created once per listener.
  const connectionRef = useRef({ baseUrl, token });
  const inFlightRef = useRef(false);

  useEffect(() => {
    connectionRef.current = { baseUrl, token };
  }, [baseUrl, token]);

  const handleUtterance = useCallback((utterance: Utterance) => {
    // The backend serialises voice commands anyway; dropping overlaps here
    // keeps a burst of speech from queueing up behind a slow model round trip.
    if (inFlightRef.current) return;
    inFlightRef.current = true;
    setSending(true);
    setLastHeard(`${utterance.durationSeconds.toFixed(1)}s of speech`);
    const { baseUrl: url, token: key } = connectionRef.current;
    void sendVoiceCommand(url, key, utterance)
      .then(() => setError(null))
      .catch((cause: unknown) =>
        setError(cause instanceof Error ? cause.message : "Voice command failed"),
      )
      .finally(() => {
        inFlightRef.current = false;
        setSending(false);
      });
  }, []);

  const startListening = useCallback(async () => {
    if (listenerRef.current || disabled) return;
    setError(null);
    const listener = new VoiceListener({
      onUtterance: handleUtterance,
      onPhase: setPhase,
      onLevel: setLevel,
      onError: setError,
    });
    try {
      await listener.start();
      listenerRef.current = listener;
    } catch (cause) {
      const message =
        cause instanceof Error ? cause.message : "Microphone unavailable";
      setError(
        /denied|NotAllowed/i.test(message)
          ? "Microphone permission denied. Allow it to let Muse listen."
          : message,
      );
    }
  }, [disabled, handleUtterance]);

  const stopListening = useCallback(async () => {
    const listener = listenerRef.current;
    listenerRef.current = null;
    await listener?.stop();
  }, []);

  useEffect(() => {
    return () => {
      void listenerRef.current?.stop();
      listenerRef.current = null;
    };
  }, []);

  const listening = phase !== "stopped";
  const statusLine = sending
    ? "Thinking…"
    : phase === "speaking"
      ? "Hearing you…"
      : listening
        ? "Listening"
        : "Not listening";

  return (
    <section className="panel">
      <h2 className="panel-title">Muse</h2>

      <div className="listen-row">
        <button
          type="button"
          className="talk"
          data-recording={phase === "speaking"}
          disabled={disabled}
          onClick={() => void (listening ? stopListening() : startListening())}
        >
          {listening ? "Stop listening" : "Start listening"}
        </button>
      </div>

      <div className="meter" aria-hidden="true">
        <div
          className="meter-fill"
          data-speaking={phase === "speaking"}
          style={{ width: `${Math.round(level * 100)}%` }}
        />
      </div>

      <p className="hint">
        <strong>{statusLine}</strong>
        {disabled
          ? " — set the backend URL and device token first."
          : listening
            ? ` — just talk. Say “Muse, play something calm”. Anything under ${MIN_UTTERANCE_SECONDS}s is ignored, and speech that was not meant for Muse changes nothing.`
            : " — one click to grant the microphone, then it stays on."}
      </p>
      {lastHeard && listening ? (
        <p className="hint">Last sent: {lastHeard}</p>
      ) : null}
      {error ? <p className="error">{error}</p> : null}
    </section>
  );
}
