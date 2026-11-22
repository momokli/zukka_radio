# Screen — MONITOR

> **Status:** Design-Ziel; die technische Basis (PCM-Tap → Opus → WebRTC) ist **M15**,
> noch nicht gebaut. Daten aus `MonitorView` (`docs/control-api.md` §5.4).

## 1. Zweck

DJs/Caster hören den Programm-Mix mit 200–500 ms und mischen sich selbst ein.
Der Operator sieht, wer verbunden ist, wie hoch die Latenz ist und ob der Cue-Bus läuft.
**5 gleichzeitige Monitore** (Entscheidung).

## 2. Layout

```
┌ Topbar (shared) ─────────────────────────────────────────────────────────┐
│ MONITOR · 2 DJs · OPUS · 140 ms · [MONITOR EIN/AUS]                      │
├──────────────────────────────┬───────────────────────────────────────────┤
│ VERBUNDEN                    │ PROGRAM / CUE                             │
│ ● Kappel   /live/slot-02     │  Programm  ▮▮▮▮▮▮▮▯▯▯  −12.4 dB           │
│   140 ms   cue: aus          │  Cue       ▮▮▯▯▯▯▯▯▯▯  −30.0 dB           │
│ ● Ankd     /live/slot-03     │                                           │
│   180 ms   cue: an           │  Latenz-Ziel: ≤ 500 ms                    │
│                              │  Codec: OPUS · 48 kHz                     │
│ (leer) „Kein DJ verbunden"   │                                           │
└──────────────────────────────┴───────────────────────────────────────────┘
```

## 3. Komponenten

| Komponente | Beschreibung |
|---|---|
| `MonitorToggle` | Monitor global ein/aus (`monitor.enable`) |
| `DjList` / `DjRow` | Name, Mount, Latenz, Cue-Status, Verbindungs-LED |
| `ProgramMeter` | Stereo-Meter des Programm-Mixes |
| `CueMeter` | Meter des Cue-Busses (Solo-Kanäle) |
| `LatencyBadge` | Ist-Latenz + Ziel |
| `CodecBadge` | `OPUS` / `PCM` |

## 4. Daten

`MonitorView`:

| Feld | Typ | UI |
|---|---|---|
| `connected_djs` | int | Zähler |
| `latency_ms` | number | Badge |
| `codec` | `opus` \| `pcm` | Badge |

**Future (mit M15):** pro DJ `{id, name, mount, latency_ms, state, cue}` — im Design
schon als Liste vorsehen.

## 5. Interaktionen

- `monitor.enable <bool>`
- `monitor.status`
- Cue/Solo: `mixer.solo <ch>` (wirkt nur auf den Cue-Bus, nie auf `program`).

## 6. Zustände

- **no_djs**: „Kein DJ verbunden".
- **connected**: Liste + Meter.
- **high_latency**: Latenz > 500 ms → Warnfarbe.
- **engine_down**: Banner, letzter Zustand bleibt sichtbar.

## 7. Visuelle Notizen

- LEDs: grün = verbunden, orange = Cue, rot = Fehler.
- Latenz groß im LCD-Font.
- Kein Blau; Cue in Teal, Warnungen in Orange.
