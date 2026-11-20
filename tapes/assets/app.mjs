// Shell bootstrap: state, feed rendering, caster filter, URL/anchor sync,
// share/download, waveform wiring (Issue #26, M3).
//
// XSS rule: dynamic values reach the DOM **only** via `textContent` /
// `setAttribute` — never `innerHTML` with tape data. The tape itself arrives
// through the server-rendered JSON island (`#tape-state`), which keeps the CSP
// at `script-src 'self'` (plan E3/T26-13).

import { Player } from "./player.mjs";
import {
  createPeaksLoader,
  drawBars,
  downsampleMax,
  paletteFrom,
  placeholderPeaks,
  barTime,
  DEFAULT_MS_PER_BAR,
} from "./peaks.mjs";

const state = {
  tapes: [],
  casters: [],
  filter: "ALL",
  nextCursor: null,
  pinned: null,
  player: null,
  loader: createPeaksLoader(),
  waveforms: new Map(),
};

const el = {
  filterbar: document.getElementById("filterbar"),
  filterCount: document.getElementById("filter-count"),
  feed: document.getElementById("feed"),
  feedEmpty: document.getElementById("feed-empty"),
  loadMore: document.getElementById("load-more"),
  player: document.getElementById("player"),
  toast: document.getElementById("toast"),
};

function readIsland() {
  const node = document.getElementById("tape-state");
  if (!node) return null;
  try {
    return JSON.parse(node.textContent);
  } catch {
    return null;
  }
}

function initialFilter() {
  const params = new URLSearchParams(window.location.search);
  return params.get("caster") || "ALL";
}

async function getJson(path) {
  const response = await fetch(path, { credentials: "same-origin" });
  if (!response.ok) throw new Error(`${path} -> ${response.status}`);
  return response.json();
}

function filterQuery() {
  return state.filter === "ALL" ? "" : `&caster=${encodeURIComponent(state.filter)}`;
}

function visibleTapes() {
  return state.tapes;
}

function tapeIndex(slug) {
  return visibleTapes().findIndex((tape) => tape.slug === slug);
}

function bySlug(slug) {
  return (
    state.tapes.find((tape) => tape.slug === slug) ||
    (state.pinned && state.pinned.slug === slug ? state.pinned : null)
  );
}

function toast(message) {
  if (!el.toast) return;
  el.toast.textContent = message;
  el.toast.hidden = false;
  window.clearTimeout(toast._timer);
  toast._timer = window.setTimeout(() => {
    el.toast.hidden = true;
  }, 2000);
}

// --- filter -----------------------------------------------------------------

function renderFilter() {
  if (!el.filterbar) return;
  el.filterbar.querySelectorAll("button[data-caster]").forEach((button) => {
    if (button.dataset.caster !== "ALL") button.remove();
  });
  for (const caster of state.casters) {
    const button = document.createElement("button");
    button.type = "button";
    button.className = "key";
    button.dataset.caster = caster.id;
    button.textContent = caster.name;
    el.filterbar.append(button);
  }
  for (const button of el.filterbar.querySelectorAll("button[data-caster]")) {
    const active = button.dataset.caster === state.filter;
    button.classList.toggle("key-active", active);
    button.setAttribute("aria-pressed", active ? "true" : "false");
  }
}

function setFilter(casterId, { push = true } = {}) {
  state.filter = casterId || "ALL";
  if (push) {
    const url = new URL(window.location.href);
    if (state.filter === "ALL") url.searchParams.delete("caster");
    else url.searchParams.set("caster", state.filter);
    window.history.replaceState({}, "", url);
  }
  renderFilter();
  void reloadFeed();
}

// --- feed -------------------------------------------------------------------

function cardId(slug) {
  return `t-${slug}`;
}

function makeCard(tape, { pinned = false } = {}) {
  const card = document.createElement("article");
  card.className = pinned ? "card card-pinned" : "card";
  card.id = cardId(tape.slug);
  card.dataset.slug = tape.slug;
  card.tabIndex = 0;
  card.setAttribute("role", "group");
  card.setAttribute("aria-label", `${tape.display_title} — Space/Enter startet oder stoppt`);
  card.setAttribute("aria-keyshortcuts", "Space Enter");

  const head = document.createElement("div");
  head.className = "card-head";

  const play = document.createElement("button");
  play.type = "button";
  play.className = "key transport";
  play.setAttribute("aria-label", "Abspielen");
  play.textContent = "\u25b6";
  play.addEventListener("click", () => playTape(tape, { autoplay: true }));
  head.append(play);

  const caster = document.createElement("button");
  caster.type = "button";
  caster.className = "card-caster";
  caster.textContent = tape.caster.name;
  caster.addEventListener("click", () => setFilter(tape.caster.id));
  head.append(caster);

  if (pinned) {
    const badge = document.createElement("span");
    badge.className = "card-badge";
    badge.textContent = "geteilt";
    head.append(badge);
  }

  const window_ = document.createElement("span");
  window_.className = "card-window";
  window_.textContent = tape.window_label;
  head.append(window_);

  const duration = document.createElement("span");
  duration.className = "card-duration";
  duration.textContent = durationLabel(tape.duration_ms);
  head.append(duration);

  const link = document.createElement("button");
  link.type = "button";
  link.className = "key card-link";
  link.textContent = "[LINK]";
  link.addEventListener("click", () => copyShare(tape));
  head.append(link);

  const download = document.createElement("a");
  download.className = "key card-download";
  download.href = tape.download_url;
  download.setAttribute("download", "");
  download.textContent = "\u2b07 MP3";
  head.append(download);

  card.append(head);

  const title = document.createElement("h2");
  title.className = "card-title";
  title.textContent = tape.display_title;
  card.append(title);

  if (tape.description) {
    const description = document.createElement("p");
    description.className = "card-desc";
    description.textContent = tape.description;
    card.append(description);
  }

  const canvas = document.createElement("canvas");
  canvas.className = "waveform";
  canvas.setAttribute("role", "slider");
  canvas.setAttribute("aria-label", "Waveform");
  canvas.setAttribute("aria-valuemin", "0");
  canvas.setAttribute("aria-valuemax", "100");
  canvas.setAttribute("aria-valuenow", "0");
  canvas.tabIndex = 0;
  card.append(canvas);

  state.waveforms.set(tape.slug, { canvas, tape, peaks: null, bars: null, hover: null, progress: 0 });
  wireWaveform(state.waveforms.get(tape.slug));

  // AK24: the card itself is the keyboard/click target. Inner controls
  // (buttons, links, canvas) keep their own behaviour.
  card.addEventListener("keydown", (event) => {
    if (event.target !== card) return;
    if (event.key === " " || event.key === "Enter") {
      event.preventDefault();
      toggleTape(tape);
    }
  });
  card.addEventListener("click", (event) => {
    if (event.target !== card && event.target.closest("button, a, canvas")) return;
    playTape(tape, { autoplay: true });
  });

  return card;
}

function durationLabel(durationMs) {
  if (!durationMs || durationMs <= 0) return "";
  const total = Math.floor(durationMs / 1000);
  const hours = Math.floor(total / 3600);
  const minutes = Math.floor((total % 3600) / 60);
  const seconds = total % 60;
  return `${hours}:${String(minutes).padStart(2, "0")}:${String(seconds).padStart(2, "0")}`;
}

// --- waveform ---------------------------------------------------------------

function wireWaveform(view) {
  const { canvas } = view;
  const palette = paletteFrom(canvas);

  const seek = (event) => {
    const rect = canvas.getBoundingClientRect();
    const ratio = Math.min(1, Math.max(0, (event.clientX - rect.left) / rect.width));
    if (!state.player) return;
    if (!state.player.tape || state.player.tape.slug !== view.tape.slug) {
      state.player.load(view.tape);
    }
    state.player.seekRatio(ratio);
  };

  canvas.addEventListener("click", (event) => {
    seek(event);
    if (state.player && state.player.tape && state.player.tape.slug === view.tape.slug) {
      state.player.play();
    }
  });

  canvas.addEventListener("mousemove", (event) => {
    const rect = canvas.getBoundingClientRect();
    view.hover = Math.min(1, Math.max(0, (event.clientX - rect.left) / rect.width));
    drawView(view);
  });
  canvas.addEventListener("mouseleave", () => {
    view.hover = null;
    drawView(view);
  });

  canvas.addEventListener("keydown", (event) => {
    if (!state.player) return;
    if (event.key === "ArrowRight") {
      event.preventDefault();
      if (!state.player.tape || state.player.tape.slug !== view.tape.slug) {
        state.player.load(view.tape);
      }
      state.player.seekDelta(10);
    } else if (event.key === "ArrowLeft") {
      event.preventDefault();
      if (!state.player.tape || state.player.tape.slug !== view.tape.slug) {
        state.player.load(view.tape);
      }
      state.player.seekDelta(-10);
    } else if (event.key === "Enter" || event.key === " ") {
      event.preventDefault();
      playTape(view.tape, { autoplay: true });
    }
  });

  const observer = new IntersectionObserver((entries) => {
    for (const entry of entries) {
      if (entry.isIntersecting) {
        observer.disconnect();
        void loadWaveform(view);
      }
    }
  });
  observer.observe(canvas);

  view.palette = palette;
  const resize = new ResizeObserver(() => drawView(view));
  resize.observe(canvas);
  drawView(view);
}

async function loadWaveform(view) {
  const { tape } = view;
  if (tape.peaks) {
    const decoded = await state.loader.load(tape.waveform_url, tape.peaks.scheme);
    if (decoded) {
      view.peaks = decoded.peaks;
      view.bars = decoded.bars;
    }
  }
  drawView(view);
}

function drawView(view) {
  const { canvas } = view;
  const width = Math.max(1, Math.floor(canvas.clientWidth));
  const height = Math.max(1, Math.floor(canvas.clientHeight));
  const dpr = window.devicePixelRatio || 1;
  if (canvas.width !== width * dpr || canvas.height !== height * dpr) {
    canvas.width = width * dpr;
    canvas.height = height * dpr;
  }
  const ctx = canvas.getContext("2d");
  ctx.setTransform(dpr, 0, 0, dpr, 0, 0);

  const source = view.peaks || placeholderPeaks(Math.max(8, width));
  const bars = downsampleMax(source, width);
  drawBars(ctx, bars, {
    width,
    height,
    progress: view.progress,
    hover: view.hover,
    colors: view.palette,
  });
  drawHoverTime(ctx, view, width, height);
  canvas.setAttribute("aria-valuenow", String(Math.round(view.progress * 100)));
}

/** `m:ss.t` — sub-second precision because one bar is 500 ms. */
function formatOffset(ms) {
  const totalTenths = Math.max(0, Math.round(ms / 100));
  const tenths = totalTenths % 10;
  const totalSeconds = Math.floor(totalTenths / 10);
  const minutes = Math.floor(totalSeconds / 60);
  const seconds = totalSeconds % 60;
  return `${minutes}:${String(seconds).padStart(2, "0")}.${tenths}`;
}

/**
 * Hover read-out (T26-18): the bar under the cursor times `ms_per_bar`.
 * Bars come from the decoded peaks; before they arrive the tape duration is
 * used so the label is still meaningful.
 */
function drawHoverTime(ctx, view, width, height) {
  if (view.hover === null) return;
  const barCount = view.peaks
    ? view.peaks.length
    : Math.ceil((view.tape.duration_ms || 0) / DEFAULT_MS_PER_BAR);
  if (barCount <= 0) return;
  const index = Math.min(barCount - 1, Math.max(0, Math.floor(view.hover * barCount)));
  const label = formatOffset(barTime(index, DEFAULT_MS_PER_BAR));
  const x = Math.min(width - 56, Math.max(4, view.hover * width - 24));
  ctx.font = '12px "Space Mono", monospace';
  ctx.textBaseline = "top";
  ctx.fillStyle = view.palette.bg || "#161c1a";
  ctx.fillRect(x - 3, 2, 52, 16);
  ctx.fillStyle = view.palette.played || "#e85a2a";
  ctx.fillText(label, x, 3);
  ctx.textBaseline = "alphabetic";
}

function refreshProgress(ratio) {
  if (!state.player || !state.player.tape) return;
  const view = state.waveforms.get(state.player.tape.slug);
  if (!view) return;
  view.progress = ratio;
  drawView(view);
}

// --- player -----------------------------------------------------------------

function playTape(tape, { autoplay = false } = {}) {
  if (!state.player) return;
  state.player.load(tape, { autoplay });
  const card = document.getElementById(cardId(tape.slug));
  if (card) highlight(card);
}

/** Starts the tape, or stops it when it is already the playing one. */
function toggleTape(tape) {
  if (!state.player) return;
  const current = state.player.tape;
  if (current && current.slug === tape.slug && !state.player.audio.paused) {
    state.player.pause();
    return;
  }
  playTape(tape, { autoplay: true });
}

function step(delta, { autoplay = false } = {}) {
  const list = visibleTapes();
  if (list.length === 0 || !state.player) return;
  const current = state.player.tape ? state.player.tape.slug : null;
  const index = current ? tapeIndex(current) : -1;
  const next = (index + delta + list.length) % list.length;
  playTape(list[next], { autoplay });
}

// --- share ------------------------------------------------------------------

async function copyShare(tape) {
  const url = tape.share_url;
  try {
    await navigator.clipboard.writeText(url);
    toast("Link kopiert");
  } catch {
    const field = document.createElement("textarea");
    field.value = url;
    field.setAttribute("readonly", "");
    field.style.position = "fixed";
    field.style.opacity = "0";
    document.body.append(field);
    field.select();
    try {
      document.execCommand("copy");
      toast("Link kopiert");
    } catch {
      toast("Kopieren nicht möglich");
    }
    field.remove();
  }
}

// --- anchors / shared tape --------------------------------------------------

function highlight(card) {
  card.classList.add("card-highlight");
  window.setTimeout(() => card.classList.remove("card-highlight"), 2000);
}

function focusTape(slug, { scroll = true, autoplay = false } = {}) {
  const tape = bySlug(slug);
  if (!tape) return false;
  playTape(tape, { autoplay });
  const card = document.getElementById(cardId(slug));
  if (card) {
    if (scroll) card.scrollIntoView({ block: "center", behavior: "smooth" });
    highlight(card);
  }
  return true;
}

function pinShared(tape) {
  if (!el.feed || !tape) return;
  state.pinned = tape;
  const card = makeCard(tape, { pinned: true });
  el.feed.prepend(card);
}

/**
 * Deep links (`#t-<slug>`) must work even when the current filter hides the
 * tape: reset to ALL, reload the feed, and only then focus the card (AK §3.9,
 * no autoplay). A slug outside the first page is loaded once by detail URL.
 */
async function handleInitialTarget(island) {
  const hash = window.location.hash;
  if (hash.startsWith("#t-")) {
    const slug = hash.slice(3);
    if (!bySlug(slug) && state.filter !== "ALL") {
      state.filter = "ALL";
      const url = new URL(window.location.href);
      url.searchParams.delete("caster");
      window.history.replaceState({}, "", url);
      renderFilter();
      await reloadFeed();
    }
    if (!bySlug(slug)) {
      try {
        const tape = await getJson(`/api/v1/tapes/${encodeURIComponent(slug)}`);
        pinShared(tape);
      } catch {
        /* unknown/hidden slug: leave the feed untouched */
      }
    }
    focusTape(slug, { scroll: true, autoplay: false });
    return;
  }
  if (island && island.slug) {
    if (!bySlug(island.slug)) pinShared(island);
    focusTape(island.slug, { scroll: true, autoplay: false });
  }
}

// --- loading ----------------------------------------------------------------

async function reloadFeed() {
  state.tapes = [];
  state.nextCursor = null;
  if (el.feed) el.feed.textContent = "";
  state.waveforms.clear();
  if (state.pinned && state.pinned.slug) {
    el.feed.append(makeCard(state.pinned, { pinned: true }));
  }
  await loadFeed();
  updateCount();
}

async function loadFeed() {
  const cursor = state.nextCursor ? `&cursor=${encodeURIComponent(state.nextCursor)}` : "";
  const data = await getJson(`/api/v1/tapes?limit=50${filterQuery()}${cursor}`);
  state.tapes = state.tapes.concat(data.tapes || []);
  state.nextCursor = data.next_cursor || null;
  for (const tape of data.tapes || []) {
    if (state.pinned && tape.slug === state.pinned.slug) continue;
    el.feed.append(makeCard(tape));
  }
  el.feedEmpty.hidden = state.tapes.length > 0;
  if (el.loadMore) el.loadMore.hidden = !state.nextCursor;
}

function updateCount() {
  if (el.filterCount) {
    el.filterCount.textContent = `${state.tapes.length}`;
  }
}

async function loadCasters() {
  try {
    state.casters = await getJson("/api/v1/casters");
  } catch {
    state.casters = [];
  }
}

async function loadMeta() {
  try {
    await getJson("/api/v1/meta");
  } catch {
    /* meta is optional for the UI */
  }
}

// --- boot -------------------------------------------------------------------

async function boot() {
  const island = readIsland();
  window.__TAPE__ = island;

  state.player = new Player(el.player, {
    onEnded: () => step(1, { autoplay: true }),
    onProgress: (ratio) => refreshProgress(ratio),
    onAction: (name) => step(name === "prev" ? -1 : 1, { autoplay: true }),
  });
  state.filter = initialFilter();

  el.filterbar?.addEventListener("click", (event) => {
    const button = event.target.closest("button[data-caster]");
    if (button) setFilter(button.dataset.caster);
  });
  el.loadMore?.addEventListener("click", () => {
    void loadFeed().then(updateCount);
  });

  await Promise.all([loadMeta(), loadCasters()]);
  renderFilter();
  await loadFeed();
  updateCount();
  await handleInitialTarget(island);
}

void boot();
