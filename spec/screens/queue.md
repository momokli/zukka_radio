# Screen — QUEUE

> Datenquelle: `zukka.db` (`queues`, `queue_items`), Portal-Routen aus `portal/src/app.rs`.
> Der Screen ist der **Playlist-/Sendungs-Editor**: Katalog + Uploads mischen, sortieren,
> einem Caster/Slot zuordnen.

## 1. Zweck

Ein Caster baut seine Sendung: Tracks aus dem Katalog, Tapes/Recordings, Links.
Reihenfolge, Titel-Override, „jetzt senden". Der Operator kann jede Queue sehen und
übernehmen.

## 2. Nutzer

- **Caster**: eigene Queues (`caster_id` = self), `kind='background'` oder `'slot'`.
- **Operator**: alle Queues, darf starten/skippen.

## 3. Layout

```
┌ Topbar (shared: ON AIR · NOW PLAYING · LISTENERS) ───────────────────────┐
│ QUEUE / [Queue-Selector ▾]  ·  total 01:42:18  ·  [AKTIVIEREN] [⋮]       │
├──────────────────────────┬───────────────────────────────────────────────┤
│ QUELLE                   │ QUEUE                                         │
│ [Suche: Titel/Artist/ISRC]│ 01 ⠿ 4:12  Artist — Title        pending  ⋮ │
│ [Alle][Tracks][Tapes]     │ 02 ⠿ 3:40  …                     playing  ⋮ │
│ [＋ Upload]               │ 03 ⠿ 0:45  Jingle: Credits        pending  ⋮ │
│ ─────────────────────    │ …                                             │
│ Ergebnisse (Liste)        │                                               │
│  + ADD                    │                                               │
├──────────────────────────┴───────────────────────────────────────────────┤
│ [▶ SENDEN] [⏭ SKIP] [⏹ LEEREN]   ·  Status: 3 pending / 1 playing        │
└──────────────────────────────────────────────────────────────────────────┘
```

**Zwei Spalten**: links Quelle (Katalog/Uploads), rechts die Queue. Auf schmalen
Screens untereinander.

## 4. Komponenten

| Komponente | Beschreibung |
|---|---|
| `QueueSelector` | Dropdown der eigenen Queues + „Neue Queue" (Name, kind) |
| `QueueMeta` | Name, `kind` (`background`/`slot`), Gesamtdauer, `version` |
| `SourceSearch` | Suchfeld (Titel/Artist/ISRC), Filter Tracks/Tapes/Links, Ergebnisliste |
| `SourceRow` | Ergebnis: Titel, Artist, Dauer, Badge (shared/private), „ADD" |
| `QueueList` | geordnete Liste, Drag&Drop oder ▲▼ |
| `QueueItem` | Position, Drag-Handle, Kind-Icon, Titel (Override), Dauer, State-Badge, Menü |
| `ItemMenu` | „nach oben/unten", „entfernen", „Titel ändern" |
| `TransportBar` | Senden/Stoppen, Skip, Leeren, Statuszeile |
| `UploadButton` | öffnet den Upload (Library-Fenster) |

## 5. Daten

`queues`:

| Feld | Typ | UI |
|---|---|---|
| `id` | string | intern |
| `name` | string | Queue-Selector / Meta |
| `kind` | `background` \| `slot` | Badge |
| `version` | int | Optimistic Locking (409) |
| `caster_id` | string | Ownership |

`queue_items`:

| Feld | Typ | UI |
|---|---|---|
| `position` | int | laufende Nummer |
| `kind` | `track` \| `tape` \| `link` | Icon |
| `track_id` / `tape_id` | ref | Titel/Dauer auflösen |
| `link_url`, `link_provider`, `link_state` | link | Badge „pending/ready/failed/unplayable" |
| `title_override` | string? | Anzeige-Titel |
| `state` | `pending` \| `playing` \| `played` \| `skipped` | State-Badge |

**Nicht im v2-Schema:** Cue-Punkte (`cue_in_ms`/`cue_out_ms`). Wenn du sie im Design
zeigst, markiere sie als „später".

## 6. Interaktionen

| Aktion | Route |
|---|---|
| Items laden / Queue öffnen | `GET /queues/{id}` |
| Item hinzufügen | `POST /queues/{id}/items` |
| Umsortieren | `POST /queues/{id}/items/{item}/move` |
| Item entfernen | `POST /queues/{id}/items/{item}/delete` |
| Umbenennen | `POST /queues/{id}/rename` |
| Skippen | `POST /queues/{id}/skip` |
| Queue löschen | `POST /queues/{id}/delete` |
| „Senden" | Broadcast starten (Playlist/Recording) → `show`-Layer |

## 7. Zustände (Design abdecken)

- **empty**: Queue ohne Items → „Quelle hinzufügen".
- **loading**: Skeleton.
- **playing**: aktuelles Item hervorgehoben, Fortschritt.
- **conflict**: `409 version_conflict` → „Queue wurde woanders geändert".
- **read-only**: fremde Queue, keine `queue.write.any` → nur ansehen.
- **link pending/failed**: Badge + „retry" (später).

## 8. Visuelle Notizen

- Tokens aus `spec/design-tokens.json`.
- State-Badges: `pending` neutral, `playing` teal, `played` gedämpft, `skipped` orange.
- Drag-Handle als Pixel-Glyph; Position im LCD-Font.
- Wenig Blau, harte Kanten, Bevel wie im Mixer.

## 9. Offene Punkte

- Cue-Punkte (nicht im Schema).
- Link-Auflösung (YouTube/Spotify → ISRC) ist ein eigener Pfad.
- Queue-Aktivierung: `settings['queue.active_id']` oder Playlist-Broadcast — im Design
  als „SENDEN" zusammenfassen.
