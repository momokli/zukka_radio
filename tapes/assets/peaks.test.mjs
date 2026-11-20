// Node built-in test runner (`node --test tapes/assets/peaks.test.mjs`), no deps.
import { test } from "node:test";
import assert from "node:assert/strict";

import {
  decodeZkwv,
  downsampleMax,
  barTime,
  placeholderPeaks,
  HEADER_BYTES,
  DEFAULT_MS_PER_BAR,
} from "./peaks.mjs";

function makeZkwv(peaks, { version = 1, magic = "ZKWV", bars = null } = {}) {
  const barCount = bars === null ? peaks.length : bars;
  const buf = new Uint8Array(HEADER_BYTES + peaks.length);
  for (let i = 0; i < 4; i += 1) buf[i] = magic.charCodeAt(i);
  buf[4] = version;
  buf[5] = 0;
  buf[6] = 0;
  buf[7] = 0;
  buf[8] = barCount & 0xff;
  buf[9] = (barCount >>> 8) & 0xff;
  buf[10] = (barCount >>> 16) & 0xff;
  buf[11] = (barCount >>> 24) & 0xff;
  buf.set(peaks, HEADER_BYTES);
  return buf;
}

test("decodes a valid ZKWV frame", () => {
  const frame = makeZkwv([0, 128, 255]);
  const decoded = decodeZkwv(frame);
  assert.equal(decoded.bars, 3);
  assert.deepEqual(Array.from(decoded.peaks), [0, 128, 255]);

  const fromBuffer = decodeZkwv(frame.buffer);
  assert.equal(fromBuffer.bars, 3);
});

test("rejects bad magic, version and length", () => {
  assert.equal(decodeZkwv(makeZkwv([1, 2], { magic: "ZZZZ" })), null);
  assert.equal(decodeZkwv(makeZkwv([1, 2], { version: 2 })), null);
  assert.equal(decodeZkwv(makeZkwv([1, 2], { bars: 5 })), null);
  assert.equal(decodeZkwv(new Uint8Array(6)), null);
  assert.equal(decodeZkwv(new Uint8Array(0)), null);
});

test("downsampleMax takes the maximum per group", () => {
  const out = downsampleMax(new Uint8Array([0, 255, 10, 20]), 2);
  assert.deepEqual(Array.from(out), [255, 20]);
});

test("downsampleMax spreads remainder groups without losing bars", () => {
  const out = downsampleMax(new Uint8Array([1, 2, 3, 4, 5]), 3);
  // groups = ceil(5/3) = 2 → [max(1,2), max(3,4), max(5)] = [2,4,5]
  assert.deepEqual(Array.from(out), [2, 4, 5]);
  assert.equal(out.length, 3);
});

test("downsampleMax is empty-safe", () => {
  const out = downsampleMax(new Uint8Array(0), 4);
  assert.deepEqual(Array.from(out), [0, 0, 0, 0]);
});

test("barTime maps a bar index to wall-clock ms", () => {
  assert.equal(barTime(0, DEFAULT_MS_PER_BAR), 0);
  assert.equal(barTime(1, DEFAULT_MS_PER_BAR), 500);
  assert.equal(barTime(10, DEFAULT_MS_PER_BAR), 5000);
  assert.equal(barTime(3), 1500);
});

test("placeholderPeaks has the requested length and non-zero values", () => {
  const peaks = placeholderPeaks(16);
  assert.equal(peaks.length, 16);
  assert.ok(peaks.some((value) => value > 0));
});
