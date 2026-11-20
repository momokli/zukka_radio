// Node built-in test runner (`node --test tapes/assets/player.test.mjs`), no deps.
import { test } from "node:test";
import assert from "node:assert/strict";

import { formatTime, resolveSeek } from "./player.mjs";

test("resolveSeek maps a ratio to seconds once the duration is known", () => {
  assert.deepEqual(resolveSeek(0.5, 120), { time: 60, pending: null });
  assert.deepEqual(resolveSeek(0, 120), { time: 0, pending: null });
  assert.deepEqual(resolveSeek(1, 120), { time: 120, pending: null });
});

test("resolveSeek clamps the ratio", () => {
  assert.deepEqual(resolveSeek(2, 100), { time: 100, pending: null });
  assert.deepEqual(resolveSeek(-1, 100), { time: 0, pending: null });
  assert.deepEqual(resolveSeek(Number.NaN, 100), { time: 0, pending: null });
});

test("resolveSeek defers until loadedmetadata (duration 0/NaN)", () => {
  assert.deepEqual(resolveSeek(0.25, 0), { time: null, pending: 0.25 });
  assert.deepEqual(resolveSeek(0.75, Number.NaN), { time: null, pending: 0.75 });
  assert.deepEqual(resolveSeek(5, 0), { time: null, pending: 1 });
});

test("formatTime renders minutes and hours", () => {
  assert.equal(formatTime(0), "00:00");
  assert.equal(formatTime(65), "01:05");
  assert.equal(formatTime(3661), "1:01:01");
});