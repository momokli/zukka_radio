// Bottom player: a single <audio>, transport, progress, volume (Issue #26, M3).
//
// No autoplay (browser policy). The player keeps the running tape even if the
// feed filter changes (docs/tapes.md §3.10). Audio playback depends on the M4
// `/media` proxy; until then the UI is fully wired but nothing streams.

/** `H:MM:SS` (or `MM:SS` under an hour). */
export function formatTime(seconds) {
  const total = Number.isFinite(seconds) && seconds > 0 ? Math.floor(seconds) : 0;
  const hours = Math.floor(total / 3600);
  const minutes = Math.floor((total % 3600) / 60);
  const secs = total % 60;
  const mm = String(minutes).padStart(2, "0");
  const ss = String(secs).padStart(2, "0");
  return hours > 0 ? `${hours}:${mm}:${ss}` : `${mm}:${ss}`;
}

export class Player {
  /**
   * @param {HTMLElement} root the `#player` element
   * @param {{onEnded?:Function,onProgress?:Function,onStateChange?:Function}} handlers
   */
  constructor(root, handlers = {}) {
    this.root = root;
    this.handlers = handlers;
    this.tape = null;
    this.duration = 0;

    this.audio = new Audio();
    this.audio.preload = "none";

    this.titleEl = root.querySelector("#player-title");
    this.timeEl = root.querySelector("#player-time");
    this.progressEl = root.querySelector("#player-progress");
    this.volumeEl = root.querySelector("#player-volume");
    this.playBtn = root.querySelector('[data-action="toggle"]');

    for (const name of ["prev", "toggle", "stop", "next"]) {
      const button = root.querySelector(`[data-action="${name}"]`);
      if (button) button.addEventListener("click", () => this.action(name));
    }

    this.audio.addEventListener("loadedmetadata", () => {
      this.duration = Number.isFinite(this.audio.duration) ? this.audio.duration : 0;
      this.updateTime();
    });
    this.audio.addEventListener("timeupdate", () => this.updateTime());
    this.audio.addEventListener("play", () => this.setPlaying(true));
    this.audio.addEventListener("pause", () => this.setPlaying(false));
    this.audio.addEventListener("ended", () => {
      this.setPlaying(false);
      if (this.handlers.onEnded) this.handlers.onEnded(this.tape);
    });

    if (this.progressEl) {
      this.progressEl.addEventListener("input", () => {
        if (!this.duration) return;
        this.audio.currentTime = (Number(this.progressEl.value) / 1000) * this.duration;
      });
    }
    if (this.volumeEl) {
      this.audio.volume = Number(this.volumeEl.value) / 100;
      this.volumeEl.addEventListener("input", () => {
        this.audio.volume = Number(this.volumeEl.value) / 100;
      });
    }

    root.addEventListener("keydown", (event) => this.onKey(event));
  }

  /** Loads a tape into the player without starting playback. */
  load(tape, { autoplay = false } = {}) {
    this.tape = tape;
    if (tape && tape.audio_url) {
      this.audio.src = tape.audio_url;
      this.audio.load();
    } else {
      this.audio.removeAttribute("src");
    }
    this.root.hidden = false;
    this.duration = 0;
    this.updateDisplay();
    this.updateTime();
    if (autoplay) this.play();
    if (this.handlers.onStateChange) this.handlers.onStateChange();
  }

  play() {
    if (!this.tape) return;
    const promise = this.audio.play();
    if (promise && typeof promise.catch === "function") promise.catch(() => {});
  }

  pause() {
    this.audio.pause();
  }

  toggle() {
    if (this.audio.paused) this.play();
    else this.pause();
  }

  stop() {
    this.audio.pause();
    try {
      this.audio.currentTime = 0;
    } catch {
      /* currentTime may be unset before metadata */
    }
    this.updateTime();
  }

  action(name) {
    switch (name) {
      case "toggle":
        this.toggle();
        break;
      case "stop":
        this.stop();
        break;
      default:
        if (this.handlers.onAction) this.handlers.onAction(name, this.tape);
        break;
    }
  }

  /** Seeks to a fraction (0..1) of the duration. */
  seekRatio(ratio) {
    if (!this.duration) return;
    const clamped = Math.min(1, Math.max(0, ratio));
    this.audio.currentTime = clamped * this.duration;
    this.updateTime();
  }

  /** Seeks relative by `seconds` (arrow keys). */
  seekDelta(seconds) {
    if (!this.duration) return;
    this.seekRatio((this.audio.currentTime + seconds) / this.duration);
  }

  onKey(event) {
    if (event.target && event.target.tagName === "INPUT") return;
    if (event.key === " " || event.key === "Enter") {
      event.preventDefault();
      this.toggle();
    } else if (event.key === "ArrowRight") {
      event.preventDefault();
      this.seekDelta(10);
    } else if (event.key === "ArrowLeft") {
      event.preventDefault();
      this.seekDelta(-10);
    }
  }

  setPlaying(playing) {
    if (this.playBtn) this.playBtn.textContent = playing ? "\u275a\u275a" : "\u25b6";
    if (this.handlers.onStateChange) this.handlers.onStateChange();
  }

  updateDisplay() {
    if (this.titleEl) {
      const tape = this.tape;
      this.titleEl.textContent = tape
        ? `${tape.caster.name} // ${tape.window_label}`
        : "";
    }
  }

  updateTime() {
    const current = this.audio.currentTime || 0;
    const total = this.duration || 0;
    if (this.timeEl) {
      this.timeEl.textContent = `${formatTime(current)} / ${formatTime(total)}`;
    }
    if (this.progressEl && total > 0) {
      this.progressEl.value = String(Math.round((current / total) * 1000));
    }
    if (this.handlers.onProgress) {
      this.handlers.onProgress(total > 0 ? current / total : 0);
    }
  }
}
