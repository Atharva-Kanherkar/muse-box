import type { Art } from "./types";

/**
 * Decodes the backend's packed 1-bit artwork into pixels.
 *
 * Format, straight from `src/render.rs`: base64 of row-major data, MSB first
 * within each byte, every row padded out to a whole byte, and a set bit means
 * ink. So row `y` starts at byte `y * ceil(w / 8)`, and pixel `x` is bit
 * `7 - (x % 8)` of byte `x / 8` within that row.
 *
 * Rendering this in the browser is what lets you see exactly what the ESP32
 * will show, rather than trusting that the packing is right.
 */
export interface DecodedArt {
  width: number;
  height: number;
  /** One byte per pixel: 1 = ink, 0 = background. */
  pixels: Uint8Array;
}

function decodeBase64(value: string): Uint8Array {
  const binary = atob(value);
  const bytes = new Uint8Array(binary.length);
  for (let index = 0; index < binary.length; index += 1) {
    bytes[index] = binary.charCodeAt(index);
  }
  return bytes;
}

export function decodeArt(art: Art): DecodedArt | null {
  if (art.w <= 0 || art.h <= 0) return null;

  let packed: Uint8Array;
  try {
    packed = decodeBase64(art.bits);
  } catch {
    return null;
  }

  const rowBytes = Math.ceil(art.w / 8);
  // The backend guarantees exactly this length; a mismatch means the document
  // and this decoder disagree, and drawing it would be misleading.
  if (packed.length !== rowBytes * art.h) return null;

  const pixels = new Uint8Array(art.w * art.h);
  for (let y = 0; y < art.h; y += 1) {
    const rowStart = y * rowBytes;
    for (let x = 0; x < art.w; x += 1) {
      const byte = packed[rowStart + (x >> 3)] ?? 0;
      const bit = (byte >> (7 - (x & 7))) & 1;
      pixels[y * art.w + x] = bit;
    }
  }
  return { width: art.w, height: art.h, pixels };
}

/**
 * Paints decoded art onto a canvas at 1:1, letting CSS scale it up with
 * `image-rendering: pixelated` so pixels stay square instead of blurring.
 */
export function paintArt(
  canvas: HTMLCanvasElement,
  decoded: DecodedArt,
  colors: { ink: string; background: string },
): void {
  const context = canvas.getContext("2d");
  if (!context) return;

  canvas.width = decoded.width;
  canvas.height = decoded.height;

  const ink = parseColor(colors.ink);
  const background = parseColor(colors.background);
  const image = context.createImageData(decoded.width, decoded.height);
  for (let index = 0; index < decoded.pixels.length; index += 1) {
    const source = decoded.pixels[index] === 1 ? ink : background;
    const offset = index * 4;
    image.data[offset] = source[0];
    image.data[offset + 1] = source[1];
    image.data[offset + 2] = source[2];
    image.data[offset + 3] = 255;
  }
  context.putImageData(image, 0, 0);
}

/** Parses `#rgb`/`#rrggbb`, falling back to mid grey rather than throwing. */
function parseColor(value: string): [number, number, number] {
  const hex = value.trim().replace(/^#/, "");
  const expanded =
    hex.length === 3
      ? hex
          .split("")
          .map((character) => character + character)
          .join("")
      : hex;
  if (!/^[0-9a-fA-F]{6}$/.test(expanded)) return [128, 128, 128];
  return [
    Number.parseInt(expanded.slice(0, 2), 16),
    Number.parseInt(expanded.slice(2, 4), 16),
    Number.parseInt(expanded.slice(4, 6), 16),
  ];
}
