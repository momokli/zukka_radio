# Cockpit v2 — Mischpult, Automation & Monitor

> **Status:** Arbeitsdokument (Brainstorm-Ergebnis). Kein Code, keine Deploy-Änderung.
> **Zweck:** Grundlage für visuelle Exploration und daraus abgeleitete Milestones/Issues.
>
> Entscheidungen des Operators (2026-11-21):
>
> 1. DJ-Monitor ist **v2-kritisch**, nicht später.
> 2. Das **Mischpult ist das Haupt-UI** des Cockpits; die „simple" Ansicht entfällt.
> 3. Filter/FX sind **nicht** v2 (später optional).
> 4. **5 gleichzeitige WebRTC-Monitore** reichen für den Start.

---

## 1. Zielbild

Ein **Mischpult**, kein Schalter. Der Operator sieht und hört die komplette Audio-Pipeline
und kann jeden Kanal greifen:

- 4–8 Kanäle gleichzeitig hörbar, N Kanal-Slots patchbar (6 Program + 32 Harbor +
  8 Relay + N Adhoc, verbindlich in `docs/control-api.md` §5.5).
- Jeder Kanal: Fader, Mute, Solo/Cue, Gain/Trim, Meter, Limiter.
- Kein Cut: Übergänge per Fade, auch vorausschauend (Set 10 s vor Ende einblenden).
- Automation entscheidet **auf Quellen-Ebene** (Schedule/Fallback), nicht per Pegel-Takeover.
- Quellen zur Laufzeit reinwerfen (Datei, HTTP-Stream, Harbor-Cast) und primär schalten.
- DJs/Caster hören den Programm-Mix **live über WebRTC** (Ziel 200–500 ms) und mischen sich
  selbst ein.

**Nicht-Ziele v2:** vollständige DAW, EQ/Kompressor pro Kanal, Hardware-FX (DJM-Ping-Pong),
Automatisations-Hüllkurven.

---

## 2. Grundmodell

```
Quellen (patchbar)
  harbor.slot-01..32   Caster (input.harbor)
  relay.slot-01..08    SomaFM / Twitch / YouTube / SRT (input.http|hls|rtmp|srt)
  queue                Scheduler-Pull (request.dynamic)
  show                 Scheduler-Pull (request.dynamic)
  rotation             playlist (request.dynamic)
  fun                  Jingle / Testton
  adhoc-<n>            Datei/Upload (Object Store) oder beliebige URL

Kanäle (Mixer)
  N Kanäle: 6 Program-Kanäle + Harbor-/Relay-Slots + Adhoc
  source → gain(ref) → mute(ref) → rms → [limiter] ─┐
                                                    ├─► mix ─► master ─► outputs
Busse                                               │
  program (öffentlich)  → icecast / hls / tape      │
  cue/monitor (privat)  → WebRTC-Return (nur Operator/DJ)  ◄─────┘
```

**Wichtig:** Es gibt **zwei Busse**:

- `program`: das, was Hörer hören (Icecast/HLS/Tape).
- `monitor`/`cue`: das, was der DJ/Operator im Kopfhörer hört (WebRTC). Cue/Solo wirkt nur hier.

**Gleichzeitigkeit:** 4–8 Kanäle sind „scharf" (im `mix`). Weitere sind gepatcht, aber
gestoppt/gemutet. `mix` dekodiert nur aktive Kanäle.

---

## 3. Kanal-Strip

| Element                | Umsetzung                       | Laufzeit                      |
| ---------------------- | ------------------------------- | ----------------------------- |
| Fader (Gain)           | `amplify({float})`              | `mixer.set <ch> <db\|linear>` |
| Mute                   | Gain-Ref auf 0 / `gate`         | `mixer.mute <ch> <bool>`      |
| Solo/Cue               | eigener Cue-Bus                 | `mixer.solo <ch> <bool>`      |
| Exclusive-Unmute       | UI-Logik + Command              | `mixer.exclusive <ch>`        |
| Meter (RMS/Peak)       | `rms(duration)` → `.rms()`      | `mixer.meters` (30 Hz)        |
| Gain Control / Limiter | `compress` / `clip`             | Konfiguration                 |
| Trim                   | zusätzlicher Gain vor dem Fader | `mixer.trim <ch> <db>`        |

EQ/Filter: **nicht v2**, optional später (`filter.iir.eq.*`).

---

## 4. Automation

Drei Ebenen, klar getrennt:

1. **Schedule/Priority (primär)** — der Scheduler entscheidet, welche Quelle _soll_.
   Die frühere `fallback`-Kette (`live > show > queue > rotation > relay > fun`) wird
   **nicht** nachgebaut: Es gibt nur den Mixer. Ein Preset setzt Kanal-Gains/-Gates
   (z. B. `live`: Harbor-Kanal auf 0 dB, alle anderen auf −∞).
2. **Übergänge** — Fades zwischen Quellen (siehe §5).
3. **Level als Werkzeug (nicht als Umschalter)** — Ducking (Musik unter Stimme),
   Fade-in. Ein kurzer Pegel-Dropout darf **nicht** zurückschalten.

Der Operator kann jederzeit **manuell übernehmen** (Override), mit sichtbarem Zustand
„Automatik aktiv / manuell".

---

## 5. Übergänge

| Fall                                                           | Operator                                        |
| -------------------------------------------------------------- | ----------------------------------------------- |
| Zusätzliche Quelle einblenden, ohne die laufende zu cutten     | `smooth_add`                                    |
| Track-zu-Track                                                 | `crossfade` / `fade.out` + `fade.in`            |
| Vorausschauend (10 s vor Ende des Podcasts den Set einblenden) | `source.on_position` + `fade.in` / `smooth_add` |
| Harte Umschaltung (Operator-Klick)                             | kurzer `fade.out`/`fade.in` (ms–Sekunden)       |

Die Fade-Dauern sind pro Kanal/Preset konfigurierbar.

---

## 6. Monitoring (WebRTC)

**Ziel:** DJs/Caster hören den Programm-Mix mit 200–500 ms und können sich selbst
gegenmischen. 5 gleichzeitige Monitore.

Architektur:

```
Liquidsoap (mix, PCM 48k stereo)
   │ output.external → Unix-Socket / Pipe (rohes PCM)
   ▼
Rust monitor-service
   │ Opus-Encode, WebRTC (str0m / webrtc-rs), Auth via Portal-Session
   ▼
DJ-Browser (5 gleichzeitig)
```

- **Eigenes Signal:** Der DJ hört sein Interface lokal (vor dem Netz) — kein Roundtrip.
- **Programm:** WebRTC-Return vom monitor-service.
- **Cue/Solo:** eigener Bus, nur im Monitor hörbar.
- **ICE/STUN/TURN:** für Remote-DJs ist TURN (`coturn`) wahrscheinlich nötig.
- **Auth:** nur eingeloggte Caster/Operatoren; ein Monitor-Token pro Session.

**Latenz-Ziele:**

| Pfad                     | Ziel                        |
| ------------------------ | --------------------------- |
| Fader → Engine           | 20–40 ms (ein Frame)        |
| Engine → WebRTC-Monitor  | 200–500 ms                  |
| Engine → Icecast (Hörer) | 1–3 s (Burst/Client-Buffer) |
| Engine → HLS             | 6–20 s                      |

Der öffentliche Stream bleibt bewusst gepuffert (Robustheit). Der Monitor ist der
„snappy" Weg.

---

## 7. Control-Protokoll

**Grundsatz:** Kein Polling im UI. Der Browser hält eine **WebSocket** zum Rust-Bridge;
der Bridge spricht mit Liquidsoap (localhost, HTTP-Command-Server) und pusht Meter/Events.

### Liquidsoap-Commands (Auszug)

```
mixer.set        <ch> <gain_db|linear>
mixer.trim       <ch> <gain_db>
mixer.mute       <ch> <true|false>
mixer.solo       <ch> <true|false>
mixer.exclusive  <ch>
mixer.patch      <ch> <file:…|https:…|harbor:/live/slot-NN|relay:…>
mixer.meters                       # JSON oder binär (30 Hz)
mixer.master     <gain_db>

layer.preset     <name>            # z. B. default | live | relay-only
layer.force      <layer|none>
layer.enable     <layer> <true|false>

monitor.enable   <true|false>
monitor.status
```

### WebSocket (UI ↔ Bridge)

- Client → Bridge: Command-Frames (JSON), ~10–50 Hz beim Fader.
- Bridge → Client: binäre Meter-Frames (Float32-Array, 30 Hz), State-Diffs, Events.

---

## 8. UI-Datenmodell

```
ChannelView {
  id, label, kind, source, connected
  gain_db, trim_db, mute, solo, exclusive
  rms_l, rms_r, peak, limiter_active
  metadata {title, artist}
}

MasterView { gain_db, limiter_active, outputs {icecast, hls, tape} }

AutomationView {
  mode: "auto" | "manual"
  preset, next_event_at, next_source
  override {channel, since}
}

MonitorView { connected_djs, latency_ms, codec }

CockpitState { channels[], master, automation, monitors, listeners, now_playing }
```

**`id` / `kind` (normativ, #152):**

| Gruppe | `id` | `kind` |
| ------ | ---- | ------ |
| Program (fix) | `live`, `show`, `queue`, `rotation`, `relay`, `fun` | gleichnamig |
| Harbor-Slots | `harbor.slot-01` … `harbor.slot-32` | `harbor` |
| Relay-Slots | `relay.slot-01` … `relay.slot-08` | `relay` |
| Adhoc | `adhoc-1`, `adhoc-2`, … | `adhoc` |

`kind` hat damit 8 Werte. `kind: "relay"` gilt für den Program-Kanal **und** die
Relay-Slots → Disambiguierung über `id`. `adhoc.file-*`/`adhoc.url-*` sind
Quell-Präfixe (`source`), nicht `id`.

**Reihenfolge `channels[]` (normativ):** erst die 6 Program-Kanäle in
Fix-Reihenfolge, dann `harbor.slot-NN` aufsteigend, dann `relay.slot-NN`
aufsteigend, dann `adhoc-<n>` aufsteigend (siehe `docs/control-api.md` §5.5).
Verbindlich auch für `mixer.meters` und den Binär-Meter-Frame.

---

## 9. Komponenten

| Komponente                  | Aufgabe                                              | Host             |
| --------------------------- | ---------------------------------------------------- | ---------------- |
| Liquidsoap                  | Mixer-Graph, Fades, Encoding, Outputs                | planet           |
| Scheduler (Rust)            | Plan, Schedule, Automation, Commands                 | planet           |
| Portal (Rust)               | Cockpit-UI, Auth, WebSocket-Proxy                    | planet           |
| monitor-service (Rust, neu) | PCM → Opus → WebRTC (5 Peers)                        | planet           |
| coturn (neu)                | TURN für Remote-DJs                                  | planet / separat |
| Icecast                     | öffentlicher Stream                                  | planet           |
| Music API                   | Object Store, ISRC-Katalog                           | `.200`           |
| Tapes                       | öffentlicher Feed, Waveforms, Medien-Proxy mit Cache | planet           |

---

## 10. Hardware (Hausnummer)

Für 4–8 aktive Kanäle, HLS, 5 WebRTC-Monitore:

| Ressource | Bedarf                                                                                                        |
| --------- | ------------------------------------------------------------------------------------------------------------- |
| CPU       | 4 Kerne ok, 8 Kerne für Luft. Decode/Mix < 1 Kern; HLS-AAC ~0,2–0,5 Kern/Stream; WebRTC ~0,1–0,3 Kern/Monitor |
| RAM       | 8 GB ok, 16 GB für Ruhe                                                                                       |
| Disk      | OS/DB klein; Recordings/Tapes groß (SSD > HDD)                                                                |
| Netz      | Uplink = Hörer × 192 kbit/s (100 ≈ 20 Mbit/s) + Monitor-Bandbreite                                            |

**Messung statt Annahme:** Decode/Encode-Benchmark auf planet und `.200` laufen lassen,
bevor wir die Engine verschieben. Empfehlung: Engine + Monitor auf planet, Object Store
auf `.200`, Cache auf planet.

---

## 11. User Stories

### Operator (Cockpit)

- Ich sehe alle Kanäle mit Pegel, Mute/Solo und Quelle.
- Ich sehe, welcher Kanal on air ist und **warum** (Automatik/Schedule/Override).
- Ich kann einen Kanal faden, muten, soloen — sofort hörbar.
- Ich kann eine Quelle zur Laufzeit patchen und primär schalten.
- Ich kann per Exclusive-Unmute alle anderen muten.
- Ich sehe Hörerzahl und Now-Playing.

### DJ/Caster

- Ich logge mich ins Portal ein und sehe mein Setup.
- Ich verbinde mein Tool mit dem Harbor und bin „verbunden, wartend" oder „live".
- Ich höre den Programm-Mix live über WebRTC und mische mich ein.
- Ich höre mein eigenes Signal lokal (kein Roundtrip).

### Hörer

- Ich höre einen kontinuierlichen Stream (Icecast/HLS), keine Cuts.
- Ich sehe Now-Playing (Titel/Artist/Show).

---

## 12. Stufenplan

1. **S1 — Mixer-Graph & Cockpit**
   - Kanäle, `mix`, Gain-Refs, `rms`, Mute/Solo, Patch.
   - Schedule/Priority-Automation als Preset (ersetzt die alte `fallback`-Kette).
   - `smooth_add`/`crossfade` für Übergänge.
   - Rust-WebSocket-Bridge + Mischpult-UI.
2. **S2 — Monitor**
   - PCM-Tap, Opus, WebRTC (5), Auth, TURN.
   - Cue/Solo-Bus, „Programm im Kopfhörer".
3. **S3 — optional**
   - Filter/FX, EQ, Automations-Hüllkurven.

---

## 13. Entscheidungen

Vom Operator bestätigt (2026-11-21): "die Empfehlungen sind gut".

- [x] **Kein Fallback/Kompatibilitätspreset**: Der Mixer ersetzt die `fallback`-Kette.
      DEV darf während der Umstellung kurz broken sein (PROD läuft weiter auf v1).
- [x] **Patching**: vorab angelegte Slots (32 Harbor, 8 Relay, N adhoc).
- [x] **Automation**: Presets (`default`/`live`/`relay-only`) + Schedule; Pegel nur für Ducking/Fade-in.
- [x] **Monitor-Transport**: direktes WebRTC (`str0m`/`webrtc-rs`) für 5 Peers; SFU erst bei Skalierung.
- [x] **TURN**: `coturn` auf planet (revisit, falls die Engine den Host wechselt).
- [x] **Cue-Bus**: eigener Mix, nur im Monitor hörbar.
- [x] **Fader-Kurve**: dB (wie Hardware).
- [x] **Meter**: 30 Hz, binäre Float32-Frames.

## 14. Noch offen

- [ ] **Visuelle Exploration** (Figma/Penpot o. ä.) auf Basis von §8.
- [ ] **Host-Placement**: Engine + Monitor auf planet vs. `.200` — erst nach Decode/Encode-Benchmark.
- [ ] **Fade-Dauern** pro Preset (Startwerte: 0,5–2 s; vorausschauend 10 s).
- [ ] **Auth des Monitors**: Session-Cookie vs. kurzlebiges Monitor-Token.

---

## 15. Risiken

- **CPU**: `mix` mit vielen aktiven Decodern; HLS-Encoding; WebRTC-Encoding.
- **Runtime-Patching**: neue Quellen nur in vorab angelegten Slots.
- **WebRTC/TURN**: NAT, Safari-Kompatibilität, Auth.
- **Konsistenz**: Plan (DB) vs. Ist (Engine) — muss im UI getrennt sichtbar sein.
- **Komplexität**: Das Cockpit wird ein Stück Software für sich; UI und Control-Bridge
  müssen zusammen designt werden, nicht nachträglich.
