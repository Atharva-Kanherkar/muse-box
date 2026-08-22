import { useCallback, useEffect, useRef, useState } from "react";
import {
  MIN_UTTERANCE_SECONDS,
  VoiceListener,
  playSpeech,
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
  const [replying, setReplying] = useState(false);
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
      .then(async (result) => {
        setError(null);
        if (!result.speech) return;
        // Deafen the listener first, or Muse's own voice becomes the next
        // utterance and it answers itself.
        listenerRef.current?.setMuted(true);
        setReplying(true);
        try {
          await playSpeech(result.speech);
        } finally {
          setReplying(false);
          listenerRef.current?.setMuted(false);
        }
      })
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
  const statusLine = replying
    ? "Muse is speaking…"
    : sending
      ? "Thinking…"
      : phase === "speaking"
        ? "Hearing you…"
        : listening
          ? "Listening"
          : "Not listening";

  const bars = 24;
  const lit = Math.round(level * bars);
  const activePhase = replying ? "replying" : sending ? "thinking" : phase;

  return (
    <section className="block">
      <h2 className="block-head">Muse</h2>

      <div className="listen">
        <div className="listen-state">
          <span className="listen-phase" data-phase={activePhase}>
            {statusLine}
          </span>
          {lastHeard && listening ? (
            <span className="log-when">{lastHeard}</span>
          ) : null}
        </div>

        <div className="levels" data-idle={!listening} aria-hidden="true">
          {Array.from({ length: bars }, (_, index) => {
            const active = listening && index < lit;
            // Bars fall away from the centre, so speech reads as a waveform
            // rather than a progress bar filling left to right.
            const falloff = 1 - Math.abs(index - (bars - 1) / 2) / (bars / 2);
            return (
              <span
                key={index}
                data-lit={active}
                style={{
                  height: active
                    ? `${8 + level * 92 * (0.35 + falloff * 0.65)}%`
                    : "3%",
                }}
              />
            );
          })}
        </div>

        <button
          type="button"
          className="control"
          data-armed={listening}
          disabled={disabled}
          onClick={() => void (listening ? stopListening() : startListening())}
        >
          {listening ? "Stop listening" : "Start listening"}
        </button>

        <p className="note">
          {disabled
            ? "Point this at the backend below before Muse can hear anything."
            : listening
              ? `Talk normally. “Muse, play something calm” pulls from your own playlists and history, not a blind search. Clips under ${MIN_UTTERANCE_SECONDS}s are ignored, and anything not meant for Muse changes nothing.`
              : "One click grants the microphone. After that it stays on — no button to hold."}
        </p>
        {error ? <p className="alert">{error}</p> : null}
      </div>
    </section>
  );
}
