// ZKWV waveform decode, downsample and canvas drawing (Issue #26, M3).
//
// Format (`docs/tapes.md` §3.6): "ZKWV" | version u8 | flags u8 | reserved u16
// | bars u32 LE | bars × u8. The client only *decodes*; the format and the
// pipeline are untouched.
//
// Downsampling uses the **maximum** per group — a mean would swallow transients
// (T26-17 / docs §3.6). Every export is a pure function so it can be tested
// with `node --test` and no DOM.

export const MAGIC = "ZKWV";
export const VERSION = 1;
export const HEADER_BYTES = 12;
export const DEFAULT_MS_PER_BAR = 500;

/**
 * Decodes a ZKWV frame.
 * @param {ArrayBuffer|Uint8Array} input
 * @returns {{bars:number, peaks:Uint8Array}|null} `null` on any header mismatch.
 */
export function decodeZkwv(input) {
  const bytes = input instanceof Uint8Array ? input : new Uint8Array(input);
  if (bytes.length < HEADER_BYTES) return null;
  if (
    bytes[0] !== 0x5a || // Z
    bytes[1] !== 0x4b || // K
    bytes[2] !== 0x57 || // W
    bytes[3] !== 0x56 // V
  ) {
    return null;
  }
  if (bytes[4] !== VERSION) return null;
  const bars =
    (bytes[8] | (bytes[9] << 8) | (bytes[10] << 16) | (bytes[11] << 24)) >>> 0;
  if (HEADER_BYTES + bars !== bytes.length) return null;
  return { bars, peaks: bytes.subarray(HEADER_BYTES) };
}

/**
 * Reduces `peaks` to `width` columns taking the maximum of each group.
 * @param {Uint8Array} peaks
 * @param {number} width
 * @returns {Uint8Array} length `width` (or fewer when there are no bars).
 */
export function downsampleMax(peaks, width) {
  const w = Math.max(1, Math.floor(width));
  const out = new Uint8Array(w);
  const bars = peaks.length;
  if (bars === 0) return out;
  const groups = Math.ceil(bars / w);
  for (let i = 0; i < w; i += 1) {
    const start = i * groups;
    if (start >= bars) break;
    const end = Math.min(start + groups, bars);
    let max = 0;
    for (let j = start; j < end; j += 1) {
      if (peaks[j] > max) max = peaks[j];
    }
    out[i] = max;
  }
  return out;
}

/**
 * Wall-clock offset (ms) of a bar.
 * @param {number} index
 * @param {number} msPerBar
 */
export function barTime(index, msPerBar = DEFAULT_MS_PER_BAR) {
  return index * msPerBar;
}

/** Deterministic placeholder pattern for cards without peaks yet. */
export function placeholderPeaks(bars) {
  const out = new Uint8Array(Math.max(0, Math.floor(bars)));
  for (let i = 0; i < out.length; i += 1) {
    out[i] = 36 + Math.round(28 * Math.abs(Math.sin(i * 0.7)));
  }
  return out;
}

/**
 * Draws mirrored bars into a 2D context.
 * @param {CanvasRenderingContext2D} ctx
 * @param {Uint8Array} bars downsampled peak bytes (one per pixel column)
 * @param {{width:number,height:number,progress?:number,hover?:number|null,colors:object}} opts
 */
export function drawBars(ctx, bars, opts) {
  const { width, height, colors } = opts;
  const progress = opts.progress ?? 0;
  const hover = opts.hover ?? null;
  const mid = height / 2;
  const count = bars.length || 1;

  if (colors.bg) {
    ctx.fillStyle = colors.bg;
    ctx.fillRect(0, 0, width, height);
  }

  const column = width / count;
  for (let i = 0; i < bars.length; i += 1) {
    const value = bars[i] / 255;
    const barHeight = Math.max(1, value * height);
    const xRatio = (i + 0.5) / count;
    let color = colors.rest;
    if (xRatio <= progress) color = colors.played;
    if (hover !== null && Math.abs(xRatio - hover) <= 1 / count) color = colors.hover;
    ctx.fillStyle = color;
    ctx.fillRect(i * column, mid - barHeight / 2, Math.max(1, column), barHeight);
  }
}

/**
 * Reads a palette from CSS custom properties with hard-coded fallbacks.
 * @param {HTMLElement} el
 */
export function paletteFrom(el) {
  const read = (name, fallback) => {
    const value = getComputedStyle(el).getPropertyValue(name).trim();
    return value || fallback;
  };
  return {
    bg: read("--wave-bg", "#161c1a"),
    rest: read("--wave-rest", "#7a7770"),
    played: read("--orange", "#e85a2a"),
    hover: read("--teal", "#2a8fa8"),
  };
}

/**
 * Fetches + decodes + caches waveforms by URL.
 * @param {typeof fetch} fetchImpl
 */
export function createPeaksLoader(fetchImpl = globalThis.fetch) {
  const cache = new Map();
  return {
    cache,
    /**
     * @param {string} waveformUrl
     * @param {number|null} scheme
     * @returns {Promise<{bars:number,peaks:Uint8Array}|null>}
     */
    async load(waveformUrl, scheme) {
      const key = `${waveformUrl}`;
      if (cache.has(key)) return cache.get(key);
      const url =
        scheme === null || scheme === undefined
          ? waveformUrl
          : `${waveformUrl}?v=${encodeURIComponent(scheme)}`;
      try {
        const response = await fetchImpl(url, { credentials: "same-origin" });
        if (!response.ok) {
          cache.set(key, null);
          return null;
        }
        const decoded = decodeZkwv(await response.arrayBuffer());
        cache.set(key, decoded);
        return decoded;
      } catch {
        cache.set(key, null);
        return null;
      }
    },
  };
}
