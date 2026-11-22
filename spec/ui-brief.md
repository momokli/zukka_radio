# UI-Brief — Cockpit v2

> Grundlage: `docs/control-api.md` (Vertrag), `docs/product-v2-cockpit.md` (Entscheidungen).
> Dieses Brief beschreibt **was** und **warum**, nicht das visuelle Detail.

## 1. Ziel

Ein **Mischpult** als Haupt-UI. Der Operator sieht die komplette Audio-Pipeline und
kann sie greifen: Fader, Mute, Solo, Gain, Meter, Quellen patchen. Automatik läuft
im Hintergrund und ist sichtbar, aber jederzeit übersteuerbar.

## 2. Nutzer

- **Operator** (Admin/Operator): alles steuern, Quellen patchen, Automatik übersteuern.
- **DJ/Caster**: eigene Verbindung sehen, Programm-Mix über WebRTC hören, sich einmischen.
- **Hörer**: sieht das Cockpit nicht; hört Stream/HLS und Now-Playing.

## 3. Screens

| Screen | Zweck |
|---|---|
| **Cockpit / Mixer** | Hauptscreen: Kanäle, Master, Automatik, Now-Playing, Hörerzahl |
| **Relay-Picker** | SomaFM-Stationen, Twitch, YouTube, SRT als Quelle wählen/patchen |
| **Queue / Playlist** | Katalog + Uploads mischen, sortieren, in Slot legen |
| **Katalog / Library** | Music-API-Suche (ISRC/Titel/Artist), Upload-Fenster |
| **Monitor** | WebRTC-Return für DJs, Latenz, Cue/Solo |
| **Tapes** | bestehende Optik (Referenz), öffentlich |

## 4. Cockpit-Layout (Vorschlag)

```
┌──────────────────────────────────────────────────────────────┐
│ Header: brand · LIVE-Badge · Hörer · Now-Playing · Automation │
├───────────────┬──────────────────────────────────────────────┤
│ CHANNELS      │  MASTER                                      │
│ [live]  ▮▮▮   │  ▮ Master-Fader                              │
│ [show]  ▮▮    │  Limiter-LED                                  │
│ [queue] ▮     │  Outputs: icecast · hls · tape               │
│ [rot]   ▮     │                                              │
│ [relay] ▮▮▮   │  MONITOR: 2 DJs · 120 ms                     │
│ [fun]   ▮     │                                              │
│ + patch…      │                                              │
└───────────────┴──────────────────────────────────────────────┘
```

Nicht auf sechs Strips begrenzt: das Cockpit zeigt **N Kanäle** (6 Program +
`harbor.slot-NN` + `relay.slot-NN` + `adhoc-<n>`) in der normativen Reihenfolge
(§5.5). Beleg-Mockup-Daten: `fixtures/wide.json`.

Jeder Kanal: Label, Quelle, Fader (dB), Meter (L/R), Mute, Solo, Exclusive,
optional Gain/Trim, Verbunden-Indikator.

## 5. Komponenten

| Komponente | Daten (State) | Interaktion |
|---|---|---|
| `ChannelStrip` | `ChannelView` | Fader → `mixer.set`; Mute → `mixer.mute`; Solo → `mixer.solo`; Exclusive → `mixer.exclusive`; Patch → `mixer.patch` |
| `Meter` | `rms_l`, `rms_r`, `peak` | read-only, 30 Hz |
| `MasterStrip` | `MasterView` | Fader → `mixer.master`; Outputs read-only |
| `AutomationBar` | `AutomationView` | Preset wählen → `layer.preset`; Force/Clear → `layer.force`; „manuell übernehmen" |
| `NowPlaying` | `now_playing` | read-only |
| `ListenerBadge` | `listeners` | read-only |
| `RelayPicker` | gepatchte Quellen | Auswahl → `mixer.patch`, dann `mixer.mute false` |
| `MonitorBadge` | `MonitorView` | read-only (später Cue/Solo) |

## 6. Interaktionsmodell

- **Fader**: vertikal, dB-Skala von −∞ … +12 dB. Drag sendet höchstens 50 Hz;
  die Bridge kollabiert auf 30 Hz.
- **Mute/Solo**: Toggle. Solo ist additiv; Exclusive ist die Ein-Klick-Variante
  („nur dieser Kanal").
- **Patch**: Quelle auswählen (Harbor-Slot, Relay-URL, Datei/Upload) → Kanal bleibt
  gemutet, bis der Operator ihn aufmacht.
- **Automatik**: Modus-Badge (`auto`/`manual`), aktives Preset, nächstes Event.
  Override („ich übernehme") ist sichtbar und kann aufgehoben werden.
- **Kein Polling**: Alles läuft über die WebSocket. Meter kommen als binäre Frames.

## 7. Zustände, die das Design abdecken muss

| Zustand | Beschreibung |
|---|---|
| `idle` | keine Quelle verbunden, alles stumm |
| `live` | ein Caster live, andere stumm |
| `crossfade` | zwei Quellen gleichzeitig hörbar (z. B. Relay + Queue) |
| `wide` | N Kanäle (>6): Program + Harbor-/Relay-Slots + Adhoc gleichzeitig sichtbar (`wide.json`) |
| `override` | Operator hat manuell übernommen |
| `engine_down` | Engine nicht erreichbar (Banner, letzter Zustand bleibt sichtbar) |
| `empty` | keine Kanäle gepatcht / keine Hörer |
| `error` | Command abgelehnt (z. B. `bad_gain`) |

Fixtures: `fixtures/idle.json`, `live.json`, `relay-crossfade.json`, `wide.json`
(>6 Kanäle: 6 Program + Harbor-/Relay-Slots + Adhoc, normative Reihenfolge).

## 8. Visuelle Sprache

- **Retro-Hardware**: Bevel, LCD, Pixel-Labels, wenig Blau, klare Buttons.
- Referenz: `tapes/assets/app.css` (Sage/Cream/Teal/Orange) und `player/index.html`
  (Kassette, LCD).
- Tokens: `design-tokens.json`.
- **Wenig Bla**: keine dekorativen Verläufe, keine Schatten mit Blur; harte Kanten.
- **Eindeutige Buttons**: Label im Pixel-Font, klarer Zustand (gedrückt/aktiv).

## 9. Technische Constraints

- Server-gerendert: **askama + htmx**, kein SPA/React.
- Live-Daten über **WebSocket** (`/api/v1/control/ws`); Meter binär, 30 Hz.
- State-Schema: `state.schema.json` (verbindlich).
- Accessibility: Keyboard-Focus, `aria-pressed` für Mute/Solo, Meter mit Textalternative.
- Responsive: Desktop-first (Operator), aber auf Tablet bedienbar.

## 10. Nicht-Ziele (v2)

- DAW-Features (Automations-Hüllkurven, Effektketten-Editor).
- EQ/Kompressor pro Kanal.
- Hardware-spezifische DJM-FX.
- Mobile-First.

## 11. Deliverables der Exploration

1. Figma-Frames für Cockpit (live, crossfade, override, engine_down).
2. Komponenten: ChannelStrip, Meter, MasterStrip, AutomationBar, RelayPicker, MonitorBadge.
3. Styles aus `design-tokens.json`.
4. Ein „Happy Path"-Prototyp: Fader bewegen, Mute, Patch, Preset wechseln.
