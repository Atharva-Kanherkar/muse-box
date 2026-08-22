import { useCallback, useEffect, useRef, useState } from "react";
import { playSpeech, sendCommand } from "../lib/voice";
import { WakeWordListener, wakeWordSupported } from "../lib/wake";

/**
 * Muse sleeps until it hears its name.
 *
 * The browser's own speech recognition does the listening, and the command
 * travels as text. That is deliberate: holding a microphone open for PCM while
 * recognition also wanted it is what made this go deaf, and shipping audio only
 * to have it transcribed again added latency, cost, and the Realtime API's
 * minimum-buffer rejections. Hardware still sends audio; a browser need not.
 */
export function VoiceControl() {
  const [awake, setAwake] = useState(false);
  const [heard, setHeard] = useState("");
  const [thinking, setThinking] = useState(false);
  const [replying, setReplying] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [typed, setTyped] = useState("");

  const wakeRef = useRef<WakeWordListener | null>(null);
  const inFlightRef = useRef(false);
  const supported = wakeWordSupported();

  const runCommand = useCallback((transcript: string) => {
    if (inFlightRef.current) return;
    inFlightRef.current = true;
    setThinking(true);
    setError(null);
    void sendCommand(transcript)
      .then(async (result) => {
        if (!result.speech) return;
        // Deafen first, or Muse's own voice becomes the next command.
        wakeRef.current?.setMuted(true);
        setReplying(true);
        try {
          await playSpeech(result.speech);
        } finally {
          setReplying(false);
          wakeRef.current?.setMuted(false);
        }
      })
      .catch((cause: unknown) =>
        setError(cause instanceof Error ? cause.message : "Muse could not act"),
      )
      .finally(() => {
        inFlightRef.current = false;
        setThinking(false);
        setHeard("");
      });
  }, []);

  const start = useCallback(() => {
    if (wakeRef.current || !supported) return;
    setError(null);
    const wake = new WakeWordListener({
      onCommand: runCommand,
      onHeard: setHeard,
      onError: setError,
    });
    if (wake.start()) {
      wakeRef.current = wake;
      setAwake(true);
    }
  }, [runCommand, supported]);

  const stop = useCallback(() => {
    wakeRef.current?.stop();
    wakeRef.current = null;
    setAwake(false);
    setHeard("");
  }, []);

  useEffect(() => {
    return () => {
      wakeRef.current?.stop();
      wakeRef.current = null;
    };
  }, []);

  const phase = replying
    ? "replying"
    : thinking
      ? "thinking"
      : heard
        ? "speaking"
        : awake
          ? "listening"
          : "stopped";
  const label = replying
    ? "Muse is speaking"
    : thinking
      ? "Thinking"
      : heard
        ? heard.slice(-46)
        : awake
          ? "Say “Muse”"
          : "Asleep";

  return (
    <div className="muse-strip">
      {supported ? (
        <>
          <button
            type="button"
            className="controlish"
            data-armed={awake}
            onClick={() => (awake ? stop() : start())}
          >
            {awake ? "Stop" : "Wake on “Muse”"}
          </button>
          <span className="muse-phase" data-phase={phase}>
            {label}
          </span>
        </>
      ) : (
        <form
          className="typed"
          onSubmit={(event) => {
            event.preventDefault();
            if (!typed.trim()) return;
            runCommand(typed.trim());
            setTyped("");
          }}
        >
          <input
            type="text"
            placeholder="Muse, play something calm"
            value={typed}
            onChange={(event) => setTyped(event.target.value)}
          />
          <button className="controlish" type="submit" disabled={thinking}>
            {thinking ? "…" : "Send"}
          </button>
        </form>
      )}
      {error ? <span className="alert">{error}</span> : null}
    </div>
  );
}
