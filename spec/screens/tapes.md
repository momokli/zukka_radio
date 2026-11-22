# Screen — TAPES

> **Status:** existiert bereits (`tapes/assets/index.html` + `tapes/assets/app.css`).
> Kein neues Design nötig — der Cockpit-Nav-Eintrag verlinkt oder bettet die Seite ein.

## 1. Zweck

Öffentlicher Feed der Aufnahmen: pro Caster filterbar, Waveform, Player, Share-Links.
Tapes sind automatisch öffentlich (`visible=1`, `status='ready'`).

## 2. Bestehende Umsetzung

- `tapes/assets/index.html` (Retro-Kassette, Filterbar, Feed, „mehr laden").
- `tapes/assets/app.css` (Tokens Sage/Cream/Teal/Orange, LCD, Waveform).
- `tapes/assets/app.mjs` + `peaks.mjs` (Feed, Waveform-Rendering).

## 3. Daten (`tapes`)

| Feld | Typ | UI |
|---|---|---|
| `slug` | string | Share-Link `/t/{slug}` |
| `title`, `description` | string? | Karte |
| `caster_id` | ref | Filter |
| `started_at`, `ended_at` | ISO | Datum/Dauer |
| `duration_ms` | int? | Player |
| `source` | `live` \| `upload` | Badge |
| `status` | `pending`/`processing`/`ready`/`failed`/`deleted` | nur `ready` öffentlich |
| `visible` | 0/1 | öffentlich |
| `storage_key`, `music_api_id` | string? | Audio-Proxy |

## 4. Cockpit-Einbindung

Im Cockpit gibt es keinen eigenen Tape-Editor (v2). Der Nav-Eintrag:

- **Option A**: öffnet `tapes.zukkafabrik.de` in einem neuen Tab.
- **Option B**: bettet die Tapes-Seite in einem Frame/Bereich ein (gleiche Optik).

Empfehlung: **A** (einfach, keine Doppelpflege). Ein „Tapes"-Detail (Upload/Veröffentlichen)
gehört in die Library, nicht ins Cockpit.

## 5. Visuelle Notizen

- Exakt die bestehende Tapes-Optik; keine neuen Tokens.
- Der Cockpit-Header bleibt sichtbar, wenn eingebettet.

## 6. Offene Punkte

- Recording-Ingest (`#132`, gemergt) → `status='ready'` erst nach Waveform.
- Waveform-Pipeline läuft im Tapes-Worker; im Cockpit nur der Link.
