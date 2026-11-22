# Screen — LIBRARY

> Datenquelle: lokale `tracks`-Projektion (aus der Music API), Uploads über
> `PUT /objects/{sha256}` (Music API), Portal-Routen aus `portal/src/app.rs`.

## 1. Zweck

Der Katalog: suchen, filtern, Details sehen, Waveform, Sichtbarkeit (shared/private),
löschen — und **hochladen** (Jingles, Bumper, fertige Sendungen, Ansagen). Upload ist
nur ein Fenster in den Object Store, kein eigenes Silo.

## 2. Nutzer

- **Caster**: sieht shared + eigene private Tracks; darf hochladen; eigene löschen.
- **Operator**: sieht alles.

## 3. Layout

```
┌ Topbar (shared) ─────────────────────────────────────────────────────────┐
│ LIBRARY · 1.284 Tracks · [＋ Upload] [⟳ Sync]                            │
├──────────────────────────────────────────────────────────────────────────┤
│ [Suche: Titel / Artist / ISRC]   [Alle][Shared][Meine][Tapes]  [Filter ▾]│
├──────────────────────────────────────────────────────────────────────────┤
│ ▸ Artist — Title           4:12   320k   shared   ▁▂▅▇▅▂▁  [⋯]          │
│ ▸ Artist — Title           3:40   FLAC   privat   ▁▃▆▇▄▁    [⋯]          │
│ ▸ Jingle: Credits          0:45   320k   shared   ▁▅▇▅▁      [⋯]          │
│ …                                                                        │
├──────────────────────────────────────────────────────────────────────────┤
│ Detail (bei Auswahl): Waveform groß · Metadaten · Sichtbarkeit · Löschen │
└──────────────────────────────────────────────────────────────────────────┘
```

## 4. Komponenten

| Komponente | Beschreibung |
|---|---|
| `SearchBar` | Titel/Artist/ISRC, debounced |
| `FilterChips` | Shared / Meine / Tapes / Format |
| `TrackRow` | Titel, Artist, Dauer, Format/Bitrate, Sichtbarkeit, Mini-Waveform, Menü |
| `Waveform` | wie Tapes (Peaks), klickbar (später Cue) |
| `TrackDetail` | großes Waveform, Metadaten, ISRC, Music-API-ID, Sichtbarkeit, Löschen |
| `UploadZone` | Drag&Drop oder Datei-Dialog; Fortschritt |
| `UploadProgress` | Job-Status (`upload_jobs`), Fehler, Dedupe |
| `ISRCImport` | ISRC eingeben → Music API ordnen/importieren |

## 5. Daten

`tracks`:

| Feld | Typ | UI |
|---|---|---|
| `title`, `artist`, `album` | string | Zeile |
| `duration_ms` | int | Dauer |
| `isrc` | string? | Detail |
| `music_api_id` | string | Detail (Object-Hash/ISRC) |
| `storage_key` | string | intern |
| `artwork_url` | string? | Cover |
| `visibility` | `shared` \| `private` | Badge |
| `caster_id` | string? | Owner |
| `metadata` | JSON | Format/Bitrate/Peaks |

Upload:

- Datei → `PUT /objects/{sha256}` (`X-Name`, `X-Original-Path`, ggf. `X-ISRC`).
- Dedupe: `HEAD /objects/{sha256}` → 200 = existiert.
- Danach lokale `tracks`-Zeile anlegen (Portal-Pipeline).

## 6. Interaktionen

| Aktion | Route / API |
|---|---|
| Suchen/Listen | `GET /library` (Query) |
| Detail | `GET /library/tracks/{id}` |
| Sichtbarkeit | `POST /library/tracks/{id}/visibility` |
| Löschen | `POST /library/tracks/{id}/delete` |
| Upload | `POST /library/upload` (multipart) + `GET /library/upload/{id}` (Poll) |
| Object Store | `PUT/HEAD /objects/{sha256}` |

## 7. Zustände

- **empty**: keine Treffer → „Suche anpassen" / „Upload".
- **uploading**: Fortschritt + Abbrechen.
- **dedupe**: „Datei existiert bereits" → verknüpfen statt neu.
- **upload error**: Grund + erneut versuchen.
- **private**: nur für Owner sichtbar; Badge.
- **waveform pending**: Platzhalter, bis Peaks da sind.

## 8. Visuelle Notizen

- Waveform wie Tapes (`--wave-rest`, `--wave-played`, `--wave-hover`).
- Format/Bitrate als LCD-Text, nicht als Fließtext.
- Upload-Zone als gestricheltes Bevel-Feld.
- Kein Blau; Badges in Teal/Orange.

## 9. Offene Punkte

- Cue-Punkte (nicht im Schema).
- Remote-Suche in der Music API (aktuell lokale Projektion; Sync-Button).
- Tapes als eigener Filter (Tapes liegen in `tapes`, nicht `tracks`).
