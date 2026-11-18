# Architektur — radio zukka v2

## 1. Vision

Ein selbstverwaltetes Community-Radio mit **voller Kontrolle** über Quellen,
Prioritäten und Übergänge. Caster können live gehen, vorproduzierte Sendungen
einplanen, Playlists kuratieren und ihre eigene Queue pflegen. Ein Operator kann
jederzeit „die Kabel umstecken".

## 2. Komponenten

```
┌──────────────────────────────────────────────────────────────┐
│  portal (Rust)                                               │
│  Auth · Upload · Library · Queue · Schedule · Control Panel  │
└───────────────┬──────────────────────────────────────────────┘
                │ HTTP (JSON)
┌───────────────▼──────────────────────────────────────────────┐
│  scheduler (Rust)                                            │
│  SQLite · Queue · Rotation · Schedule · Prioritäten          │
│  Live-Auswahl · Announcements                                │
└───────────────┬──────────────────────────────────────────────┘
                │ HTTP (request.dynamic, control)
┌───────────────▼──────────────────────────────────────────────┐
│  liquidsoap                                                  │
│  input.harbor (mehrere Caster) · request.dynamic · playlist  │
│  switch · fallback · crossfade · record · output             │
└───────────────┬──────────────────────────────────────────────┘
                │
┌───────────────▼──────────────────────────────────────────────┐
│  icecast  +  caddy (HLS-Segmente, statische Dateien)         │
└──────────────────────────────────────────────────────────────┘
```

**Extern konsumiert:**

```
music-api  ←  Object Storage + Track-Autorität (ISRC, Downloads)
music-hub  ←  Multi-User-Track-Verwaltung
```

## 3. Kernbegriffe

| Begriff | Definition |
| --- | --- |
| **Show** | Optionales Format: Name, Beschreibung, Artwork, Links, Default-Schedule |
| **Broadcast** | Slot mit Inhalt: `live`, `recording` oder `playlist` |
| **Source** | Layer, der gerade ausgegeben wird |
| **Queue** | Pro Caster, benannt, läuft kontinuierlich im Hintergrund |
| **Tape / Recording** | Live mitgeschnitten oder hochgeladen — dasselbe |
| **Caster** | Portal-User + Streamer-Credentials |
| **Operator** | Rolle mit erweiterten Rechten (Live-Auswahl, Caster-Verwaltung) |

### Show vs. Broadcast

- Eine **Show** ist optional. Ein **Broadcast** existiert immer.
- Ein Broadcast kann auf eine Show verweisen (`show_id`) oder nicht.
- Titel-Logik: manuell > (später: Zeit-Regel) > DJ-Standard > Fallback.

## 4. Prioritäten (Layer)

```
100  live      Caster über Harbor
 80  show      Recording oder Playlist-Show
 60  queue     pro Caster, benannt
 40  rotation  Basis-Programm
 20  relay     SomaFM Groove Salad → andere SomaFM
  1  fun       SpongeBob-Credits-Loop
```

- Defaults, später pro Source konfigurierbar.
- Es läuft immer die **höchste verfügbare** Ebene.
- Wird eine höhere verfügbar → umschalten.
- Fällt sie weg → nächste Ebene.

## 5. Live-Verhalten

- Mehrere Caster können **parallel** verbunden sein (je eigener Harbor-Mount).
- Genau **ein** Mount wird ausgegeben.
- Reihenfolge: **first come, first served**.
- Disconnect → automatisch der nächste Wartende.
- Nahtloser Handoff (Crossfade).
- **Kein Delay.**
- Operator kann manuell umschalten.
- Kein Kick/Bann — nur „wer ist live".

## 6. Content & Storage

- **Music API = Object Storage für alles**: Tracks, Recordings, Tapes, Jingles, Shows.
- Gemeinsame **und** private Library (Semantik noch zu klären).
- Jeder darf Playlists für Slots kuratieren.
- Alle Formate; bei Bedarf konvertiert.
- Keine Limits.

## 7. Announcements

- **Events:** live on/off, Show-Reminder, Recording veröffentlicht, Fehler/Fallback.
- **Timings:** global + pro Show überschreibbar.
- **Templates:** Platzhalter (`{show_name}`, `{caster_name}`, `{start_time}`, …), pro Show anpassbar.
- **Channel:** ein Haupt-Channel (Telegram), später erweiterbar.
- **Posten:** System **und** Caster manuell.

## 8. Zugriff & Rollen

- Jeder darf Shows anlegen und Broadcasts planen (Piratenradio).
- Admin/Operator verwalten Caster.
- **RBAC von Anfang an**, permissiv by default.
- Details der Rechte-Matrix später.

## 9. Zeit

- **Immer Berlin** (`Europe/Berlin`), DST-korrekt.
- Alle Zeitfenster, Schedule-Logs und Announcements in Berliner Zeit.
- Kein Hardcoding von UTC-Offsets.

## 10. Offene Punkte

- Shared vs. private Library — genaue Semantik.
- Music-API-Integration (ISRC vs. IDs).
- Upload-Flow im Detail.
- Rollen/Rechte-Matrix.
- Titel-Zeitregeln.
- „Du bist live"-Kommunikation (UI/Telegram).
- HLS-Encoding-Details.
- Relay-Konfiguration (SomaFM).
