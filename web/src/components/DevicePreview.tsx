import { useEffect, useRef, useState } from "react";
import { decodeArt, paintArt } from "../lib/art";
import {
  MAX_RENDER_DIMENSION,
  MIN_RENDER_DIMENSION,
  type DitherMode,
  type RenderDoc,
  type RenderParams,
} from "../lib/types";

interface Props {
  doc: RenderDoc | null;
  params: RenderParams;
  onParamsChange: (params: RenderParams) => void;
}

/**
 * The 1-bit frame the ESP32 actually receives, unpacked and painted at native
 * resolution. It is the signature of the whole device, so it leads the page —
 * and a bit-order regression in the backend shows up here as visible noise
 * rather than passing silently.
 */
export function DevicePreview({ doc, params, onParamsChange }: Props) {
  const canvasRef = useRef<HTMLCanvasElement>(null);
  // A set bit means ink, but whether ink is the lit pixel or the dark one
  // depends on the panel: emissive by default, inverted for e-paper.
  const [inkIsLit, setInkIsLit] = useState(true);
  const art = doc?.art ?? null;

  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas || !art) return;
    const decoded = decodeArt(art);
    if (!decoded) return;
    const accent = doc?.palette[1] ?? "#e0e0e0";
    const dominant = doc?.palette[0] ?? "#1a1a1a";
    paintArt(canvas, decoded, {
      ink: inkIsLit ? accent : dominant,
      background: inkIsLit ? dominant : accent,
    });
  }, [art, doc?.palette, inkIsLit]);

  function clamp(value: string, fallback: number): number {
    const parsed = Number.parseInt(value, 10);
    if (!Number.isFinite(parsed)) return fallback;
    return Math.min(MAX_RENDER_DIMENSION, Math.max(MIN_RENDER_DIMENSION, parsed));
  }

  return (
    <section className="block">
      <h2 className="block-head">Panel</h2>

      <div className="panel-stage">
        {art ? (
          <canvas
            ref={canvasRef}
            className="panel-canvas"
            aria-label={`Dithered ${art.w} by ${art.h} device frame`}
          />
        ) : (
          <div className="panel-empty">Awaiting frame</div>
        )}
      </div>

      {art ? (
        <dl className="readout">
          <div>
            <dt>Size</dt>
            <dd>
              {art.w}×{art.h}
            </dd>
          </div>
          <div>
            <dt>Dither</dt>
            <dd>{art.dither}</dd>
          </div>
          <div>
            <dt>Packed</dt>
            <dd>{Math.ceil(art.w / 8) * art.h} B</dd>
          </div>
        </dl>
      ) : null}

      <div className="dials">
        <label className="field">
          Width
          <input
            type="number"
            inputMode="numeric"
            min={MIN_RENDER_DIMENSION}
            max={MAX_RENDER_DIMENSION}
            value={params.w}
            onChange={(event) =>
              onParamsChange({ ...params, w: clamp(event.target.value, params.w) })
            }
          />
        </label>
        <label className="field">
          Height
          <input
            type="number"
            inputMode="numeric"
            min={MIN_RENDER_DIMENSION}
            max={MAX_RENDER_DIMENSION}
            value={params.h}
            onChange={(event) =>
              onParamsChange({ ...params, h: clamp(event.target.value, params.h) })
            }
          />
        </label>
        <label className="field">
          Dither
          <select
            value={params.dither}
            onChange={(event) =>
              onParamsChange({
                ...params,
                dither: event.target.value as DitherMode,
              })
            }
          >
            <option value="bayer">bayer</option>
            <option value="atkinson">atkinson</option>
          </select>
        </label>
        <label className="field">
          Ink
          <select
            value={inkIsLit ? "lit" : "dark"}
            onChange={(event) => setInkIsLit(event.target.value === "lit")}
          >
            <option value="lit">lit</option>
            <option value="dark">dark</option>
          </select>
        </label>
      </div>
    </section>
  );
}
