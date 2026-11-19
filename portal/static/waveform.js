// Client waveform renderer (Issue #23, D8).
//
// Selects every `.waveform-canvas` (`<canvas data-waveform-src="/library/tracks/{id}/waveform.json">`),
// lazily loads the raw peaks via IntersectionObserver and paints the interleaved
// 8-bit (min, max) bands onto the canvas.
//
// Contract of the JSON payload (unchanged, produced since #18):
//   { subject_kind, subject_id, scheme, algorithm, ms_per_bar, bars, data: [u8; 2*bars] }
// `data` is interleaved (min, max) pairs; `0x80` is silence. The renderer never
// depends on `ms_per_bar`/`bars` semantics beyond the pair count.
//
// Idempotent for htmx swaps: every `htmx:afterSwap` re-scans the swapped subtree.
(function () {
  "use strict";

  // Fallback height used when the element has no measured CSS height yet.
  var FALLBACK_HEIGHT = 40;
  // `WeakSet`s keep repeated scans (htmx swaps) idempotent.
  var observed = typeof WeakSet === "function" ? new WeakSet() : null;
  var loaded = typeof WeakSet === "function" ? new WeakSet() : null;
  // Canvases with decoded peaks, so a resize can repaint at the new width.
  var drawn = [];

  function accent() {
    try {
      var value = getComputedStyle(document.documentElement).getPropertyValue("--accent");
      if (value && value.trim()) return value.trim();
    } catch (err) {
      /* fall through */
    }
    return "#f2a33c";
  }

  function size(canvas) {
    var dpr = window.devicePixelRatio || 1;
    var cssWidth = canvas.clientWidth;
    if (!cssWidth && canvas.parentElement) cssWidth = canvas.parentElement.clientWidth;
    if (!cssWidth) cssWidth = 200;
    var cssHeight = canvas.clientHeight || FALLBACK_HEIGHT;
    canvas.width = Math.max(1, Math.round(cssWidth * dpr));
    canvas.height = Math.max(1, Math.round(cssHeight * dpr));
    var ctx = canvas.getContext("2d");
    // Draw in CSS pixels; the DPR transform keeps it crisp.
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    return { ctx: ctx, width: cssWidth, height: cssHeight };
  }

  function draw(canvas, bars, data) {
    var surface = size(canvas);
    var ctx = surface.ctx;
    ctx.clearRect(0, 0, surface.width, surface.height);
    if (!bars || !data || data.length < bars * 2 || surface.width <= 0) return;

    var mid = surface.height / 2;
    var barWidth = surface.width / bars;
    ctx.fillStyle = accent();
    for (var i = 0; i < bars; i++) {
      var lo = (data[2 * i] - 128) / 128;
      var hi = (data[2 * i + 1] - 128) / 128;
      if (lo > hi) {
        var swap = lo;
        lo = hi;
        hi = swap;
      }
      var top = mid - hi * mid;
      var bottom = mid - lo * mid;
      var band = Math.max(1, bottom - top);
      ctx.fillRect(i * barWidth, top, Math.max(1, barWidth * 0.9), band);
    }
  }

  function hide(canvas) {
    canvas.hidden = true;
  }

  function load(canvas) {
    if (loaded && loaded.has(canvas)) return;
    if (loaded) loaded.add(canvas);

    var src = canvas.getAttribute("data-waveform-src");
    if (!src) {
      hide(canvas);
      return;
    }
    if (typeof fetch !== "function") {
      hide(canvas);
      return;
    }

    fetch(src, { headers: { Accept: "application/json" } })
      .then(function (response) {
        if (!response.ok) throw new Error("waveform " + response.status);
        return response.json();
      })
      .then(function (payload) {
        if (!payload || !payload.bars || !payload.data || !payload.data.length) {
          hide(canvas);
          return;
        }
        canvas.hidden = false;
        canvas.__waveformPeaks = { bars: payload.bars, data: payload.data };
        draw(canvas, payload.bars, payload.data);
        if (drawn.indexOf(canvas) === -1) drawn.push(canvas);
      })
      .catch(function () {
        // 404 / network error: leave the canvas blank, never throw.
        hide(canvas);
      });
  }

  function observe(canvas) {
    if (observed) {
      if (observed.has(canvas)) return;
      observed.add(canvas);
    } else if (canvas.__waveformObserved) {
      return;
    } else {
      canvas.__waveformObserved = true;
    }

    if (typeof IntersectionObserver === "undefined") {
      load(canvas);
      return;
    }
    var observer = new IntersectionObserver(function (entries) {
      entries.forEach(function (entry) {
        if (entry.isIntersecting) {
          observer.unobserve(entry.target);
          load(entry.target);
        }
      });
    });
    observer.observe(canvas);
  }

  function scan(root) {
    var scope = root && root.querySelectorAll ? root : document;
    var nodes = scope.querySelectorAll(".waveform-canvas");
    Array.prototype.forEach.call(nodes, observe);
  }

  function redraw() {
    drawn.forEach(function (canvas) {
      if (!canvas.isConnected) return;
      if (!canvas.__waveformPeaks) return;
      draw(canvas, canvas.__waveformPeaks.bars, canvas.__waveformPeaks.data);
    });
  }

  function init() {
    scan(document);
    document.body.addEventListener("htmx:afterSwap", function (evt) {
      scan(evt.target);
    });
    window.addEventListener("resize", redraw);
  }

  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", init);
  } else {
    init();
  }
})();
