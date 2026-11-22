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

## Bekannte Lücke (bitte beim Design beachten)

`docs/control-api.md` modelliert aktuell **6 Layer-Kanäle** (`live`, `show`, `queue`,
`rotation`, `relay`, `fun`). Das Produkt braucht aber **N patchbare Mixer-Kanäle**
(32 Harbor-Slots, 8 Relay-Slots, Adhoc). Die Erweiterung ist als Issue angelegt
(„Multi-Channel-Modell“):

- `ChannelView.kind` wird um `harbor`, `relay`, `adhoc` erweitert (oder `id`-Präfixe).
- `id`: `harbor.slot-01`…, `relay.slot-01`…, `adhoc-<n>`; die festen Program-Kanäle
  (`queue`, `rotation`, `show`, `fun`) bleiben.
- `mixer.patch`, `mixer.meters` und `CockpitState` müssen N Kanäle tragen.

Für die **visuelle Exploration** gilt schon das Zielmodell: **mehrere Kanal-Strips**,
nicht sechs. Die Fixtures zeigen vorerst die sechs dokumentierten Kanäle; ein
`wide`-Fixture folgt mit dem Issue.

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
