import { useCallback, useEffect, useRef, useState } from "react";
import {
  VoiceListener,
  playSpeech,
  sendVoiceCommand,
  type ListenerPhase,
  type Utterance,
} from "../lib/voice";
import { WakeWordListener, wakeWordSupported } from "../lib/wake";

interface Props {
  baseUrl: string;
  token: string;
  disabled: boolean;
}

/**
 * Muse sleeps until it hears its name.
 *
 * The microphone stays open, but nothing leaves the browser until the wake word
 * fires — otherwise music in the room reads as speech and every song becomes a
 * command. Where the browser has no speech recognition there is an explicit
 * talk button instead, which is honest about the limitation rather than
 * silently streaming the room.
 */
export function VoiceControl({ baseUrl, token, disabled }: Props) {
  const [phase, setPhase] = useState<ListenerPhase>("stopped");
  const [level, setLevel] = useState(0);
  const [sending, setSending] = useState(false);
  const [replying, setReplying] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [woke, setWoke] = useState(false);

  const listenerRef = useRef<VoiceListener | null>(null);
  const wakeRef = useRef<WakeWordListener | null>(null);
  const connectionRef = useRef({ baseUrl, token });
  const inFlightRef = useRef(false);
  const supported = wakeWordSupported();

  useEffect(() => {
    connectionRef.current = { baseUrl, token };
  }, [baseUrl, token]);

  const handleUtterance = useCallback((utterance: Utterance) => {
    if (inFlightRef.current) return;
    inFlightRef.current = true;
    setWoke(false);
    setSending(true);
    const { baseUrl: url, token: key } = connectionRef.current;
    void sendVoiceCommand(url, key, utterance)
      .then(async (result) => {
        setError(null);
        if (!result.speech) return;
        // Deafen both listeners first, or Muse's own voice trips the wake word
        // and it answers itself.
        listenerRef.current?.setMuted(true);
        wakeRef.current?.setMuted(true);
        setReplying(true);
        try {
          await playSpeech(result.speech);
        } finally {
          setReplying(false);
          listenerRef.current?.setMuted(false);
          wakeRef.current?.setMuted(false);
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

  const start = useCallback(async () => {
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
    } catch (cause) {
      const message =
        cause instanceof Error ? cause.message : "Microphone unavailable";
      setError(
        /denied|NotAllowed/i.test(message)
          ? "Microphone permission denied. Allow it to let Muse listen."
          : message,
      );
      return;
    }
    listenerRef.current = listener;

    if (supported) {
      const wake = new WakeWordListener({
        onWake: () => {
          setWoke(true);
          listenerRef.current?.arm();
        },
        onHeard: () => {},
        onError: setError,
      });
      wake.start();
      wakeRef.current = wake;
    }
  }, [disabled, handleUtterance, supported]);

  const stop = useCallback(async () => {
    wakeRef.current?.stop();
    wakeRef.current = null;
    const listener = listenerRef.current;
    listenerRef.current = null;
    await listener?.stop();
    setWoke(false);
  }, []);

  useEffect(() => {
    return () => {
      wakeRef.current?.stop();
      void listenerRef.current?.stop();
      wakeRef.current = null;
      listenerRef.current = null;
    };
  }, []);

  const awake = phase !== "stopped";
  const activePhase = replying
    ? "replying"
    : sending
      ? "thinking"
      : phase === "speaking"
        ? "speaking"
        : awake
          ? "listening"
          : "stopped";
  const statusLine = replying
    ? "Muse is speaking"
    : sending
      ? "Thinking"
      : phase === "speaking"
        ? "Listening to you"
        : awake
          ? supported
            ? "Asleep — say “Muse”"
            : "Ready"
          : "Off";

  return (
    <div className="muse-strip">
      <button
        type="button"
        className="controlish"
        data-armed={awake}
        disabled={disabled}
        onClick={() => void (awake ? stop() : start())}
      >
        {awake ? "Stop" : "Wake on “Muse”"}
      </button>

      <span className="muse-phase" data-phase={activePhase}>
        {statusLine}
      </span>

      <span className="mini-levels" data-idle={!awake} aria-hidden="true">
        {Array.from({ length: 12 }, (_, index) => {
          const active = awake && index < Math.round(level * 12);
          return (
            <span
              key={index}
              data-lit={active && woke}
              style={{ height: active ? `${20 + level * 80}%` : "12%" }}
            />
          );
        })}
      </span>

      {awake && !supported ? (
        <button
          type="button"
          className="controlish"
          disabled={sending}
          onClick={() => listenerRef.current?.arm()}
        >
          Talk
        </button>
      ) : null}

      {error ? <span className="alert">{error}</span> : null}
    </div>
  );
}
