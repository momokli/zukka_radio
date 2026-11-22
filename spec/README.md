# Spec-Bundle — Cockpit v2

Dieses Verzeichnis ist ein **Import-Bundle für UI-Werkzeuge** (Figma, Penpot, v0, …).
Es beschreibt, _was_ das Cockpit zeigt und _welche Daten_ es bekommt — nicht, wie es
implementiert wird.

## Normativ vs. Inspirierend

| Datei                           | Rolle                                                          |
| ------------------------------- | -------------------------------------------------------------- |
| `../docs/control-api.md`        | **normativ**: API-Vertrag (Commands, State, WebSocket)         |
| `../docs/product-v2-cockpit.md` | **normativ**: Produkt-/Architekturentscheidungen               |
| `state.schema.json`             | maschinenlesbares State-Schema (abgeleitet aus dem Vertrag)    |
| `commands.json`                 | maschinenlesbarer Command-Katalog                              |
| `fixtures/*.json`               | Beispiel-Zustände (Inhalt für Mockups/Prototypen)              |
| `ui-brief.md`                   | Design-Brief: Ziele, Screens, Komponenten, Interaktion, States |
| `design-tokens.json`            | Farben/Typografie/Form aus Tapes + Player                      |
| `../tapes/assets/`              | Referenz-Optik (Tapes-Seite, CSS)                              |
| `../player/`                    | Referenz-Optik (Kassette, Player)                              |

## Kanalmodell (Vertrag, kein Ausblick mehr)

Das Multi-Channel-Modell ist mit **Issue #152** in den normativen Vertrag
(`docs/control-api.md` §5.1/§5.5, `state.schema.json`) eingezogen. Es gilt:

- `ChannelView.kind` hat **8 Werte**: `live`, `show`, `queue`, `rotation`, `relay`,
  `fun`, `harbor`, `adhoc` (`state.schema.json` `$defs.ChannelKind.enum`).
- `id`: die festen Program-Kanäle (`live`, `show`, `queue`, `rotation`, `relay`,
  `fun`) plus Slot-Kanäle `harbor.slot-01…`, `relay.slot-01…` und `adhoc-<n>`.
  Das Schema validiert die `id` per `pattern`.
- `mixer.patch`, `mixer.meters` und `CockpitState.channels` tragen **N** Kanäle
  (nicht mehr fix 6). `schema_version` bleibt `1` (additive Erweiterung, §1.1).
- Reihenfolge (`CockpitState.channels`, `mixer.meters`, Binär-Frame): erst die
  sechs Program-Kanäle in Fix-Reihenfolge, dann `harbor.slot-NN`, dann
  `relay.slot-NN`, dann `adhoc-<n>` — jeweils aufsteigend (§5.5).

Für die **visuelle Exploration** gilt jetzt genau dieses Zielmodell: **mehrere
Kanal-Strips**, nicht sechs. Belege dafür sind `fixtures/wide.json` (>6 Kanäle,
alle Gruppen) sowie die drei weiterhin gültigen 6-Kanal-Fixtures.

## Empfohlenes Werkzeug

**Figma** für die visuelle Exploration:

1. `ui-brief.md` als Brief lesen, `fixtures/*.json` als echten Inhalt in die Frames
   legen (kein Lorem Ipsum).
2. Komponenten aus `ui-brief.md` §5 als Figma-Components anlegen.
3. `design-tokens.json` als Styles/Variables importieren.
4. `tapes/assets/index.html` + `player/index.html` als Screenshot-Referenz.

**Penpot** ist die Open-Source-Alternative, wenn du selbst hosten willst.

**Nicht empfohlen für diesen Stack:** React-Codegeneratoren (v0/Lovable). Das Portal
ist server-gerendert (askama + htmx), kein SPA. Wenn du damit experimentierst, nimm
nur HTML/CSS mit, nicht die React-Struktur.

## Nächster Schritt danach

Aus dem Design leiten wir die Umsetzung ab: HTML/askama + htmx gegen genau dieses
State-Schema und den WebSocket-Vertrag. Die Debug-UI aus M14 (#149) ist der
Funktionstest; das echte Cockpit ersetzt sie.
