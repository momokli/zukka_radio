# Rollback-Plan (Issue #47)

Dieser Plan beschreibt den **vollständigen Rückweg** des PROD-Stacks — vom
v2-internen Deploy-Rollback bis „zurück zum alten Stack". Er bündelt die
**Entscheidung** (welche Ebene wann, durch wen, mit welchem Zeitbudget) und
**verweist** für jede Mechanik auf ihre Single Source of Truth. Betriebs-
mechanik wird hier **nicht dupliziert** (siehe „Querverweise" unten).

Grundlage: `docs/consolidated-architecture.md` §5.7–5.9 (SoT).

## Querverweise — was hier NICHT steht

| Thema                                                              | Quelle (SoT)                              | In diesem Plan nur |
| ------------------------------------------------------------------ | ----------------------------------------- | ------------------ |
| Deploy-/Rollback-Mechanik (`.deployed`/`.deployed.prev`, Auto-Rollback) | `docs/deploy-prod.md` „Rollback"          | Verweis            |
| Rollback-Fenster, Trigger, Edge/DNS-Rückweg-**Reihenfolge**        | `docs/cutover.md` §8                      | Verweis            |
| Legacy-Import-Rückweg (`.bak`-Dateiaustausch)                      | `docs/data-migration.md` §6               | Verweis            |
| Snapshot-Restore → `zukka_data`                                    | `docs/backup.md` „Notfall-Restore"        | Verweis            |
| Pre-Deploy-Snapshot im Deploy-Pfad                                 | `docs/backup.md` „Pre-Deploy-Snapshot"    | Verweis            |

Dieser Plan ergänzt ausschließlich die **Entscheidung**: welches Symptom welche
Ebene auslöst, wer sie autorisiert, wie lange sie dauern darf und wie der
Erfolg verifiziert wird. Voraussetzung (SoT): Migrationen sind **expand/contract**
(`consolidated-architecture.md` §5.7), damit eine alte Binary mit dem neuen
Schema starten kann.

## Ebenenmodell

| Ebene | Name                    | Was sie rückgängig macht                                    | Reichweite                          |
| ----- | ----------------------- | ----------------------------------------------------------- | ----------------------------------- |
| **L1** | Deploy-Rollback (v2-intern) | den letzten Tag-/Image-Wechsel der v2-Software         | v2-Control-Plane, gleiche DB/Schema |
| **L2** | Daten-/Migrations-Rollback | den Legacy-Import bzw. eine fehlerhafte Datenänderung   | die SQLite-DB auf dem `zukka_data`-Volume |
| **L3** | Cutover-Rollback        | den gesamten Cutover — **zurück zum alten Stack**           | Edge, DNS, Bestandssystem, v2-Stack |

**Reihenfolge-Grundsatz:** Der kleinste hinreichende Eingriff zuerst. L1 ist
billig und reversibel; L2 tauscht Daten; L3 gibt die Plattform zurück. Bei
kombiniertem Schaden gilt: **erst die passende Software-Ebene (L1), dann Daten
(L2)** — nie umgekehrt (siehe L2 „Schema-Mismatch").

## Entscheidungsmatrix

Zeilen = beobachtetes Symptom; die Spalte „Mechanik" verweist nur.

| Symptom (beobachtet)                                        | Ebene | Trigger                                   | Autorität                    | Mechanik (Verweis)                   | RTO       | Verifikation                          |
| ---------------------------------------------------------- | ----- | ----------------------------------------- | ---------------------------- | ------------------------------------ | --------- | ------------------------------------- |
| Smoke-Gate nach Tag-Wechsel rot; Healthz/`readyz` rot       | L1    | automatisch (roter Smoke)                 | Operator (Runbook)           | `deploy-prod.md` „Rollback"          | ≤ 10 min  | Smoke, `/readyz`, `.deployed`-SHA     |
| Crash-Loop / Fehlerbild unmittelbar nach Tag-Wechsel        | L1    | manuell (Operator)                        | Operator                     | `deploy-prod.md` „Rollback"          | ≤ 10 min  | Smoke, `docker compose ps`            |
| Waisen/Duplikate, FK-Fehler nach `--apply`                  | L2    | manuell (Operator)                        | Projekt-Owner (Daten)        | `data-migration.md` §6               | ≤ 30 min  | `readyz` + Zähl-/Stichproben-Check    |
| Falsches Mapping / Datenverlust-Verdacht                    | L2    | manuell (Operator)                        | Projekt-Owner (Daten), Restore 4-Augen | `data-migration.md` §6 / `backup.md` | ≤ 30 min  | `readyz` + Integritätsprüfung         |
| Binär-/Schema-Mismatch (DB passt nicht zum Tag)             | L1 → L2 | manuell                                | Projekt-Owner                | erst L1, dann `data-migration.md` §6 | ≤ 10 + ≤ 30 min | Smoke, dann DB-Check              |
| `/stream.mp3` ≠ 200, fremde Domain tot, `ZukkaAudioDown`    | L3    | (siehe `cutover.md` §8)                   | Owner **+** Host-Owner       | `cutover.md` §8                      | ≤ 60 min  | `cutover.md` §7                       |
| v2 kann den Dienst gar nicht erbringen                     | L3    | manuell (Operator)                        | Owner **+** Host-Owner       | `cutover.md` §8                      | ≤ 60 min  | `cutover.md` §7                       |

## RTO / Zeitbudget

| Ebene | Budget    | Anmerkung                                                            |
| ----- | --------- | -------------------------------------------------------------------- |
| L1    | ≤ 10 min  | ein Image-`pull` + Recreate + Smoke (v2-intern)                      |
| L2    | ≤ 30 min  | deckungsgleich mit `backup.md` „Notfall-Restore (< 30 min)"          |
| L3    | ≤ 60 min  | innerhalb des Cutover-Fensters + 24 h (`cutover.md` §8)              |

RTO gemessen ab **Entscheidung**, nicht ab Symptom-Auftreten.

## Entscheidungsbaum

1. **Cutover bereits erfolgt?** Nein → es gibt nur L1/L2; L3 existiert vor dem
   Cutover nicht (der alte Stack ist noch live). Weiter mit 2.
2. **Ist der Deploy/Tag defekt** (Smoke rot, Crash-Loop, Fehlerbild nach
   Tag-Wechsel) **und** DB/Schema passen? → **L1**.
3. **Sind Daten/Schema defekt** (Waisen, FK-Fehler, falsches Mapping,
   Datenverlust)? → **L2**. Passt die DB **nicht** zum Tag (Mismatch) → **erst
   L1** auf den zur DB passenden Tag, **dann L2**.
4. **Ist Edge/DNS/Altsystem betroffen, oder kann v2 den Dienst gar nicht
   erbringen?** → **L3** („zurück zum alten Stack"). Vorher v2 einfrieren:
   Deploys stoppen, `.deployed`-SHA notieren, Ad-hoc-Backup ziehen.

## L1 — Deploy-Rollback (v2-intern)

Setzt die **Mechanik** aus `docs/deploy-prod.md` „Rollback" voraus (dort
dokumentiert; hier nur Ablauf + Entscheidung).

**Runbook (manuell):**

```bash
sudo /usr/local/bin/zukka-deploy prod --rollback
```

**Auto-Rollback:** Schlägt das Smoke-Gate eines normalen Deploys fehl und
existiert ein `.deployed` mit SHA ≠ fehlgeschlagenem Tag, deployt der Wrapper
automatisch den Stand aus `.deployed` (den zuletzt erfolgreich ausgerollten,
aktuell guten Stand — **nicht** `.deployed.prev`) und bricht danach mit Exit
≠ 0 ab.

**`.deployed.prev`-Semantik:** `.deployed.prev` ist der **letzte bekannt gute**
Stand. Ein Rollback **rotiert nicht**: er deployt `.deployed.prev` und setzt
**nur** `.deployed`. Deshalb ist ein zweiter `--rollback` **idempotent**
(No-op, Exit 0) — kein Zurück-Togglen auf den defekten Tag.

**Vorbedingungen:**

- Migrationen sind **expand/contract** (alte Binary startet mit neuem Schema).
- Pre-Deploy-Snapshot für den guten Stand vorhanden (`backup.md`).
- `.deployed.prev` **existiert** (sonst kein L1 möglich → Fehlermeldung
  „no …/.deployed.prev to roll back to").

**Trigger:** Smoke rot; Healthz/`readyz` rot; Crash-Loop; Fehlerbild
unmittelbar nach Tag-Wechsel.

**Grenzen:** Ist `.deployed.prev` **selbst defekt**, oder passt die DB/das
Schema nicht zum Ziel-Tag, hilft L1 nicht → **L2** (bei Mismatch erst L1 auf den
passenden Tag, dann L2) bzw. **L3**.

**Verifikation:** Smoke-Gate grün; `/readyz` = 200; `docker compose ps` alle
`running (healthy)`; `.deployed`-SHA = erwarteter Rollback-SHA.

**Nachweis:** `scripts/zukka-rollback-test.sh` (Drill gegen lokale Fake-Umgebung)
und CI-Job `rollback` in `.github/workflows/ci.yml`.

## L2 — Daten-/Migrations-Rollback

Zwei Wege, Entscheidungsregel voran:

- **(a) `.bak`-Dateiaustausch** — schnellster Weg, **nur** wenn das `.bak`
  unmittelbar vor dem Import entstand (`VACUUM INTO '<db>.bak'`,
  `data-migration.md` §6). Da die Migration rein app-seitig importiert (keine
  Schemaänderung), ist der Rückweg ein reiner Dateiaustausch.
- **(b) restic-Restore** des passenden Snapshots (`predeploy-<sha>` bzw. täglich)
  via `backup.md` „Notfall-Restore (< 30 min)" — wenn kein passendes `.bak`
  existiert oder der Zustand älter ist.

**Entscheidungsregel:** `.bak` vorhanden und unmittelbar vor dem Import erzeugt?
→ (a). Sonst → (b).

**Ablauf (a):** Scheduler/Portal/Tapes stoppen → `zukka.db` durch `<db>.bak`
ersetzen → `-wal`/`-shm` entfernen → starten → `readyz` + Zähl-/Stichproben-
Verifikation. Konkretes Kommando siehe `data-migration.md` §6.

**Schema-Mismatch:** Schema-Migrationen sind **append-only**. Passt die DB nach
dem Import nicht zum ausgerollten Tag, **zuerst L1** auf den zur DB passenden
Tag, **dann L2**. Nie eine alte DB unter eine inkompatible Binary zwingen.

**Trigger:** Waisen/Duplikate; FK-Fehler (`foreign_key_check`); falsches Mapping
nach `--apply`; Datenverlust-Verdacht.

**Autorität:** Projekt-Owner (Daten); bei Restore **Vier-Augen-Prinzip**.

**RTO:** ≤ 30 min (deckungsgleich `backup.md`).

**Verifikation:** `readyz` = 200; Zählung/Stichproben gegen die Quelle;
`PRAGMA integrity_check`/`foreign_key_check` leer.

## L3 — Cutover-Rollback (zurück zum alten Stack)

L3 gibt die Plattform an das Bestandssystem zurück. Die operative
Edge/DNS-**Reihenfolge** und die Trigger stehen in `docs/cutover.md` §8 — hier
nur Entscheidung, Autorität, Voraussetzungen und Fenster.

**Reihenfolge (Kurzfassung, kanonisch `cutover.md` §8):** Fremd-Domains zuerst
sichern — `import`-Zeile/Snippet entfernen → `caddy validate` → Edge-Recreate →
DNS auf den notierten Alt-Zielwert → Bestandssystem starten → `cast`-Record
**zuletzt** → Verifikation (`cutover.md` §7).

**Autorität:** Projekt-Owner **und** Host-Owner **gemeinsam** (Edge/DNS/
Bestandssystem sind geteiltes Eigentum).

**Fenster:** Cutover-Fenster (03:00–04:00 Europe/Berlin) **plus 24 h**. Innerhalb
des Fensters ohne Diskussion; danach bis 04:00 des Folgetags
**begründungspflichtig** (`cutover.md` §8).

**Kein automatischer DNS-Rollback:** DNS ist manuell (Cloudflare). Der
Rück-Wert wird in der Ausführungs-Checkliste dokumentiert; **kein** Skript rollt
DNS zurück.

**Voraussetzungen:** Der alte DNS-Zielwert ist **notiert**; das Bestandssystem
ist **noch nicht dekommissioniert**. Sind diese nicht erfüllt, ist L3 nur mit
Owner-Freigabe und Wiederanlauf des Altsystems möglich.

**Vor dem L3-Beginn (Einfrieren):** Deploys stoppen, `.deployed`-SHA notieren,
Ad-hoc-Backup ziehen (`backup.md`).

**Verifikation:** wie `cutover.md` §7 (`/stream.mp3` = 200, HLS jünger als
3× Segmentdauer, `/healthz` grün, Fremd-Domains unverändert, `smoke.sh prod`
grün).

## Eskalation

- **L1 zweimal rot** → L2 bzw. L3 in Erwägung ziehen (L1 erschöpft).
- **L2 rot** → restic-Restore des passenden Snapshots (`backup.md`).
- **L3** → Host-Owner führt den manuellen Edge/DNS-Rückweg aus.
- Jede Eskalation wird mit Zeitstempel, Symptom, gewählter Ebene und Autorität
  im Incident-Log (`docs/monitoring.md`) festgehalten.

## Nachweis & CI

- **Drill:** `scripts/zukka-rollback-test.sh` beweist die L1-Zustandsmaschine
  (Rollback ohne `.prev`, Rotation genau einmal, idempotenter zweiter Rollback,
  Auto-Rollback bei rotem Smoke, `recover_state`-Promotion, fail-closed
  Pre-Deploy-Backup) gegen eine **lokale Fake-Umgebung** (kein PROD, kein root,
  kein echtes Docker).
- **CI:** Job `rollback` in `.github/workflows/ci.yml` (analog `backup`), in
  `build.needs` eingebunden.
