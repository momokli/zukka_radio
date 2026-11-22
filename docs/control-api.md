# Control API — Vertrag (v1)

**Status:** verbindlich (M14, Issue #144).
**Gilt für:** Engine (Liquidsoap), Scheduler (Rust), Bridge (Rust/Portal), Cockpit-UI,
spätere Automation. Die übrigen M14-Issues implementieren **exakt** diesen Vertrag.

Dieses Dokument ist die *eine* Quelle für:

1. Liquidsoap-Commands (Name, Argumente, Rückgabe, Fehler).
2. State-JSON (`ChannelView`, `MasterView`, `AutomationView`, `MonitorView`, `CockpitState`).
3. WebSocket-Protokoll (Client → Bridge, Bridge → Client).
4. Fehlercodes und Versionierung.

Grundsatz (siehe `docs/product-v2-cockpit.md` §7): **kein Polling im UI.** Der Browser
hält eine WebSocket zur Bridge; die Bridge spricht mit Liquidsoap (localhost,
HTTP-Command-Server, Port 8010) und pusht Meter/Events.

---

## 1. Versionierung

| Ebene             | Träger                                   | Aktuell |
| ----------------- | ---------------------------------------- | ------- |
| HTTP/REST         | Pfadpräfix `/api/v1`                     | `v1`    |
| WebSocket         | Feld `api_version` im `hello`-Event      | `1`     |
| Liquidsoap-Command | keine Version im Command; Vertrag stabil | `1`     |
| State-JSON        | Feld `schema_version` in `CockpitState`  | `1`     |

Regeln:

- **Additiv** ist kompatibel: neue optionale Felder/Events/Commands dürfen ohne
  Versionssprung dazukommen. Clients **müssen** unbekannte Felder ignorieren.
- **Brechend** (Feld umbenannt, Typ geändert, Semantik invertiert) erhöht
  `schema_version` / `api_version` und wird in diesem Dokument vermerkt.
- Der Server verwirft Commands mit unbekannter `api_version` nicht still, sondern
  antwortet mit `version_mismatch` (§4).

### 1.1 Changelog

| Datum      | Issue | Änderung | Versionswirkung |
| ---------- | ----- | -------- | --------------- |
| 2026-11-22 | #152  | Multi-Channel-Modell: `ChannelView.kind` um `harbor`/`adhoc` erweitert (Enum-Widening auf 8 Werte), `id`-Grammatik für Slot-Kanäle dokumentiert, `CockpitState.channels`-Reihenfolge normativ als Gruppenordnung, N-generische Formulierung für `mixer.patch`/`mixer.meters`/Binär-Meter-Frame | **`schema_version` bleibt `1`** |

**Begründung „kein Versionssprung“ (Issue #152):** Das Erweitern eines `enum` um
zusätzliche Werte sowie zusätzliche `channels[]`-Einträge sind *additiv* im Sinne
dieser Regeln — bestehende Clients, die unbekannte `kind`/`id`-Werte ignorieren,
bleiben funktionsfähig, und die sechs Program-Kanäle behalten ID, Bedeutung und
Position. Die Präzisierung der `channels[]`-Reihenfolge (Gruppenordnung statt
laxem „`id` sortiert“, §5.5) ändert die *Formulierung*, nicht das drahtseitige
Schema: sie war für die sechs Program-Kanäle bereits identisch und wird nun für
N Kanäle eindeutig festgeschrieben. Damit ist kein Feld umbenannt, kein Typ
geändert und keine Semantik invertiert → **keine** Erhöhung von `schema_version`.

---

## 2. Transport & Auth

### 2.1 HTTP (REST)

- Basis: `http://<host>:<port>/api/v1`.
- Content-Type: `application/json; charset=utf-8`.
- Schreibende Endpunkte liefern `204 No Content` bei Erfolg; lesende `200 OK` mit JSON.
- Auth: Portal-Service-Token (§4.1 consolidated-architecture) + Permission
  `control.read` / `control.write` (§6.10.1). Fehlt der Token → `401`, fehlt die
  Berechtigung → `403`.

### 2.2 WebSocket (Bridge)

- URL: `ws(s)://<host>:<port>/api/v1/control/ws`.
- Auth: derselbe Token; Übergabe als erster Frame `{"type":"auth","token":"…"}` **oder**
  per `Authorization: Bearer …`-Header beim Upgrade. Der Server schließt die Verbindung
  mit Close-Code `4401` bei fehlender/ungültiger Auth.
- Textframes = JSON. Binärframes = Meter (§6.2).
- Ping/Pong: der Server sendet alle 15 s einen Ping; ein Client ohne Pong innerhalb
  von 30 s wird mit Close-Code `4408` getrennt.

---

## 3. Fehler-Envelope (REST)

Jeder Fehler hat dieselbe Form (consolidated-architecture §4.6):

```json
{
  "error": {
    "code": "validation_error",
    "message": "gain_db out of range",
    "details": { "field": "gain_db", "min": -60.0, "max": 12.0 }
  }
}
```

- `code` ist maschinenlesbar und stabil; `message` ist für Menschen und darf sich ändern.
- `details` ist optional und objektförmig.

### 3.1 Fehlercode-Tabelle

| HTTP | `code`                 | Bedeutung                                                        |
| ---- | ---------------------- | ---------------------------------------------------------------- |
| 400  | `bad_request`          | Malformed/Unparseable Request                                    |
| 401  | `unauthorized`         | Token fehlt/ungültig                                             |
| 403  | `forbidden`            | Authentifiziert, aber ohne Permission                            |
| 404  | `not_found`            | Ressource/Route fehlt (z. B. `channel_not_found`)                |
| 409  | `conflict`             | Zustandskonflikt (siehe Codes unten)                             |
| 422  | `validation_error`     | Feldvalidierung fehlgeschlagen                                   |
| 429  | `too_many_requests`    | Rate-Limit (Portal D3)                                           |
| 500  | `internal_error`       | Unerwarteter Serverfehler (Details leaken nie)                   |

Konflikt-Subcodes (`409`, Feld `code`):

| `code`            | Auslöser                                                       |
| ----------------- | -------------------------------------------------------------- |
| `version_conflict`| Optimistic Locking verletzt (erwartete Version ≠ Ist)          |
| `live_active`     | `pause`/`switch` gegen laufenden Live-Broadcast                |
| `queue_in_use`    | Queue-Änderung während laufender Wiedergabe (falls zutreffend) |
| `duplicate_name`  | Eindeutiger Name doppelt                                       |
| `duplicate_position` | Eindeutige Position doppelt                                 |

---

## 4. Liquidsoap-Commands

Alle Commands werden als **Body** eines `POST http://127.0.0.1:8010/control` gesendet;
der Response-Body ist der Rückgabewert (Text). Leerer Body = `OK`. Fehler des
Command-Servers beginnen mit `ERROR` und werden als `command_failed` gemappt.

### 4.1 Mixer / Kanal

| Command                      | Argumente                                   | Rückgabe        | Fehler |
| ---------------------------- | ------------------------------------------- | --------------- | ------ |
| `mixer.set <ch> <gain>`      | `ch` = Kanal-ID; `gain` = `dB` (z. B. `-6.0`) oder `linear` (0.0–1.0) | `OK` | `unknown_channel`, `bad_gain` |
| `mixer.trim <ch> <gain_db>`  | `ch`; `gain_db` in dB                        | `OK`            | `unknown_channel`, `bad_gain` |
| `mixer.mute <ch> <true\|false>` | `ch`; bool                               | `OK`            | `unknown_channel` |
| `mixer.solo <ch> <true\|false>` | `ch`; bool                               | `OK`            | `unknown_channel` |
| `mixer.exclusive <ch>`       | `ch` (unmutet genau diesen Kanal, mutet alle anderen) | `OK` | `unknown_channel` |
| `mixer.patch <ch> <source>`  | `ch` = **jede** Kanal-ID (§5.5); `source` = `file:<pfad>` \| `https:<url>` \| `harbor:/live/slot-NN` \| `relay:<url>` | `OK` | `unknown_channel`, `bad_source` |
| `mixer.master <gain_db>`     | `gain_db` in dB                              | `OK`            | `bad_gain` |
| `mixer.meters`               | keine                                        | JSON-Array (siehe §4.5) oder Binär | – |

Semantik:

- `gain` bei `mixer.set`: `dB`-Wert (negativ = leiser) **oder** `linear` als
  Dezimalzahl 0.0–1.0. `mixer.set ch 0.0` entspricht linear stumm; `-inf`/`mute`
  ist `mixer.mute`.
- `mixer.trim` ist ein zusätzlicher Gain **vor** dem Fader (Trim-Regler).
- `mixer.solo` schaltet den Kanal auf den Cue-Bus; mehrere Solos sind erlaubt
  (Oder-Verknüpfung). `mixer.exclusive` ist die UI-Abkürzung „nur dieser".
- `mixer.patch` tauscht die Quelle eines Kanals zur Laufzeit; der Kanal bleibt
  gemutet, bis `mixer.mute … false` kommt.
- N-generisch: `<ch>` ist **jede** in `CockpitState.channels` vorkommende Kanal-ID
  (§5.5) — die sechs Program-Kanäle **oder** Slot-Kanäle (`harbor.slot-NN`,
  `relay.slot-NN`, `adhoc-<n>`). Die Quellen-Grammatik ist unverändert und
  unabhängig von der Kanalzahl.

### 4.2 Layer / Automation

| Command                              | Argumente                                   | Rückgabe | Fehler |
| ------------------------------------ | ------------------------------------------- | -------- | ------ |
| `layer.preset <name>`                | `name` ∈ `default` \| `live` \| `relay-only` | `OK` | `unknown_preset` |
| `layer.force <layer\|none>`          | `layer` ∈ `live` \| `show` \| `queue` \| `rotation` \| `relay` \| `fun`; `none` hebt auf | `OK` | `unknown_layer` |
| `layer.enable <layer> <true\|false>` | `layer`; bool                               | `OK`     | `unknown_layer` |

`layer.preset` setzt Kanal-Gains/-Gates gemäß dem dokumentierten Preset
(`docs/product-v2-cockpit.md` §4). `layer.force` ist der manuelle Override der
Automatik; `none` gibt an die Automatik zurück.

### 4.3 Monitor

| Command                     | Argumente        | Rückgabe | Fehler |
| --------------------------- | ---------------- | -------- | ------ |
| `monitor.enable <true\|false>` | bool          | `OK`     | –      |
| `monitor.status`            | keine            | JSON (siehe §5.4) | – |

### 4.4 Live / Tape (bestehend, stabil)

| Command              | Argumente        | Rückgabe | Fehler |
| -------------------- | ---------------- | -------- | ------ |
| `live.set <mount>`   | Mount (`/live/slot-NN`) | `OK` | `bad_mount` |
| `live.clear`         | keine            | `OK`     | –      |
| `live.status`        | keine            | JSON `{live_order, override, current}` | – |
| `tape.start <id>`    | Tape-ID          | `OK`     | `not_found` |
| `tape.stop`          | keine            | `OK`     | –      |
| `status`             | keine            | JSON (Engine-Status) | – |
| `health`             | keine            | `OK`     | –      |

### 4.5 `mixer.meters` — Rückgabe

`mixer.meters` liefert ein JSON-Array, **ein Objekt pro Kanal** (N-generisch,
N = Länge von `CockpitState.channels`) in exakt der Reihenfolge von
`CockpitState.channels` (§5.5):

```json
[
  { "id": "live", "rms_l": -18.2, "rms_r": -18.0, "peak": -3.1, "limiter_active": false },
  { "id": "show", "rms_l": -32.0, "rms_r": -32.4, "peak": -12.0, "limiter_active": false }
]
```

- `rms_l`/`rms_r`/`peak` in dBFS (`-inf` als `null`).
- `limiter_active` = `true`, wenn der Gain-Control/Limiter des Kanals eingreift.
- Das Array hat immer genau N Einträge in `channels[]`-Ordnung; `id` identifiziert
  den Kanal (nicht die Position).

### 4.6 HTTP Command-Proxy (Scheduler)

Der Scheduler exponiert einen **HTTP-Command-Proxy** (`POST /api/v1/control/command`),
der einen einzelnen Engine-Command validiert, an den Liquidsoap-Command-Server
pusht und Engine-/Transport-Fehler auf die Vertrags-Codes aus §6.4 mappt. Der
Proxy spiegelt §4.1–§4.4, akzeptiert aber nur schreibende Commands
(`mixer.set|trim|mute|solo|exclusive|patch|master`, `layer.preset|force|enable`,
`monitor.enable`, `live.set|clear`, `tape.start|stop`). Read-Commands
(`mixer.meters`, `live.status`, `layer.status`, `monitor.status`) sind **nicht**
über den Proxy erreichbar; sie dienen intern der State-Projektion.

Die Fehlercodes aus §6.4 werden auf HTTP-Status abgebildet; `error.code` ist der
Vertrags-Code:

| `code`               | HTTP | Auslöser                                                     |
| -------------------- | ---- | ----------------------------------------------------------- |
| `unknown_command`    | 400  | `cmd` nicht in der Proxy-Whitelist                          |
| `channel_not_found`  | 404  | Engine `unknown_channel` (grammatik-gültige, unbekannte ID) |
| `invalid_args`       | 422  | fehlende/ungültige `args`                                   |
| `bad_gain`           | 422  | Gain außerhalb `[-60,+12]` dB bzw. linear außerhalb `[0,1]` |
| `bad_source`         | 422  | ungültiges `source`-Präfix                                  |
| `command_failed`     | 502  | Engine-Body `ERROR …` ohne bekanntes Sub-Code               |
| `engine_unavailable` | 503  | kein Endpoint konfiguriert **oder** Transportfehler/Timeout |

`bad_gain`/`invalid_args` prüft der Scheduler **vor** dem Push (die Engine
prüft die dB-Range nicht, `#145`-Finding); die Range-Checks sind damit
fail-fast. `gain_db` und `linear` schließen sich aus.

Der Scheduler liest den Kanalzustand nicht zurück (kein `mixer.state`); die
`channels[]`-Felder `gain_db`/`mute`/`solo`/`exclusive` in §5.1 sind daher eine
**Plan-Projektion** (persistierte Absicht) und die Pegel (`rms_*`/`peak`)
stammen aus `mixer.meters` (fail-open).

---

## 5. State-JSON

Alle Views sind **read-only Snapshots**. Zeitstempel sind ISO-8601 UTC (`Z`).
Numerische Pegel sind dBFS, sofern nicht anders vermerkt.

### 5.1 `ChannelView`

```json
{
  "id": "live",
  "label": "Live",
  "kind": "live",
  "source": "harbor:/live/slot-01",
  "connected": true,
  "gain_db": -6.0,
  "trim_db": 0.0,
  "mute": false,
  "solo": false,
  "exclusive": false,
  "rms_l": -18.2,
  "rms_r": -18.0,
  "peak": -3.1,
  "limiter_active": false,
  "metadata": { "title": "…", "artist": "…" }
}
```

| Feld             | Typ              | Bedeutung |
| ---------------- | ---------------- | --------- |
| `id`             | string           | stabile Kanal-ID (Grammatik unten) |
| `label`          | string           | Anzeigename |
| `kind`           | enum             | `live` \| `show` \| `queue` \| `rotation` \| `relay` \| `fun` \| `harbor` \| `adhoc` |
| `source`         | string \| null   | aktuell gepatchte Quelle (`file:`/`https:`/`harbor:`/`relay:`) |
| `connected`      | bool             | Quelle liefert Daten |
| `gain_db`        | number \| null   | Fader in dB (`null` = linear/stumm) |
| `trim_db`        | number           | Trim-Gain in dB |
| `mute`           | bool             | stummgeschaltet |
| `solo`           | bool             | auf Cue-Bus |
| `exclusive`      | bool             | einziger hörbarer Kanal |
| `rms_l`,`rms_r`  | number \| null   | RMS dBFS (`null` = kein Signal) |
| `peak`           | number \| null   | Peak dBFS |
| `limiter_active` | bool             | Limiter greift ein |
| `metadata`       | object \| null   | `{title, artist}` |

#### `id`-Grammatik und `kind`-Zuordnung

Die Kanal-ID kodiert Rolle und – bei Slot-Kanälen – den Slot. `kind` ist die
Klassifikation für die UI; `id` ist stabil und identifiziert den Kanal eindeutig.

| Gruppe | `id`-Muster | `kind` | Bedeutung |
| ------ | ----------- | ------ | --------- |
| Program (fix) | `live`, `show`, `queue`, `rotation`, `relay`, `fun` | jeweils gleichnamig | die sechs festen Program-Layer, immer vorhanden |
| Harbor-Slots | `harbor.slot-01` … `harbor.slot-32` (2-stellig) | `harbor` | Caster-Eingänge (`input.harbor`) |
| Relay-Slots | `relay.slot-01` … `relay.slot-08` (2-stellig) | `relay` | externe Streams (SomaFM/Twitch/YouTube/SRT) |
| Adhoc | `adhoc-1`, `adhoc-2`, … (1-basiert) | `adhoc` | Datei/Upload oder URL |

Hinweise:

- `kind: "relay"` kommt bei **zwei** Gruppen vor: dem Program-Kanal `id: "relay"`
  **und** den Slot-Kanälen `relay.slot-NN`. Disambiguiert wird über `id`.
- Die kanonische Adhoc-ID ist `adhoc-<n>`; `adhoc.file-*`/`adhoc.url-*` sind
  Quell-Präfixe (`source`), **keine** `id`.
- Die Program-Kanäle behalten ID, `kind` und Position unverändert.

### 5.2 `MasterView`

```json
{
  "gain_db": 0.0,
  "limiter_active": false,
  "outputs": { "icecast": true, "hls": true, "tape": false }
}
```

| Feld             | Typ    | Bedeutung |
| ---------------- | ------ | --------- |
| `gain_db`        | number | Master-Fader in dB |
| `limiter_active` | bool   | Master-Limiter greift ein |
| `outputs`        | object | `{icecast, hls, tape}` — je bool |

### 5.3 `AutomationView`

```json
{
  "mode": "auto",
  "preset": "default",
  "next_event_at": "2026-11-22T20:00:00Z",
  "next_source": "show",
  "override": { "channel": "live", "since": "2026-11-22T18:12:00Z" }
}
```

| Feld            | Typ              | Bedeutung |
| --------------- | ---------------- | --------- |
| `mode`          | enum             | `auto` \| `manual` |
| `preset`        | string \| null   | aktives Preset (`default`/`live`/`relay-only`) |
| `next_event_at` | string \| null   | ISO-8601 des nächsten geplanten Events |
| `next_source`   | string \| null   | Kanal-ID des nächsten Events |
| `override`      | object \| null   | `{channel, since}` bei manuellem Override |

### 5.4 `MonitorView`

```json
{
  "connected_djs": 2,
  "latency_ms": 120,
  "codec": "opus"
}
```

| Feld            | Typ    | Bedeutung |
| --------------- | ------ | --------- |
| `connected_djs` | number | Anzahl verbundener Remote-DJs |
| `latency_ms`    | number | geschätzte Monitor-Latenz |
| `codec`         | string | `opus` \| `pcm` |

### 5.5 `CockpitState`

```json
{
  "schema_version": 1,
  "generated_at": "2026-11-22T19:42:10Z",
  "channels": [ /* ChannelView[] */ ],
  "master": { /* MasterView */ },
  "automation": { /* AutomationView */ },
  "monitors": { /* MonitorView */ },
  "listeners": 42,
  "now_playing": { "title": "…", "artist": "…", "source": "rotation" }
}
```

| Feld             | Typ            | Bedeutung |
| ---------------- | -------------- | --------- |
| `schema_version` | number         | Vertragsversion dieses Dokuments (`1`) |
| `generated_at`   | string         | Snapshot-Zeitpunkt (ISO-8601 UTC) |
| `channels`       | ChannelView[]  | alle Kanäle, stabile Reihenfolge (Gruppenordnung, siehe unten) |
| `master`         | MasterView     | Master |
| `automation`     | AutomationView | Automatik |
| `monitors`       | MonitorView    | Monitor |
| `listeners`      | number \| null | Hörerzahl (Icecast), `null` = unbekannt |
| `now_playing`    | object \| null | `{title, artist, source}` |

`CockpitState` ist der Body von `GET /api/v1/control/state` und zugleich der
Voll-Snapshot, den die Bridge beim WebSocket-`hello` pusht.

**Reihenfolge von `channels[]` (normativ):** Die Liste trägt N Kanäle in vier
Gruppen, in dieser festen Ordnung:

1. **Program (fix)** in genau dieser Reihenfolge: `live`, `show`, `queue`,
   `rotation`, `relay`, `fun`.
2. **Harbor-Slots** `harbor.slot-01` … `harbor.slot-NN` aufsteigend.
3. **Relay-Slots** `relay.slot-01` … `relay.slot-08` aufsteigend.
4. **Adhoc** `adhoc-1` … `adhoc-<n>` aufsteigend.

Diese Ordnung ist die einzige verbindliche Sortierung — sie ersetzt das frühere
laxe „`id` sortiert“ (eine lexikografische Sortierung würde `adhoc-*` vor
`live` einsortieren, was **nicht** gewollt ist). Dieselbe Reihenfolge gilt für
`mixer.meters` (§4.5) und den binären Meter-Frame (§6.2).

---

## 6. WebSocket-Protokoll

### 6.1 Client → Bridge (JSON-Commands)

Ein Command-Frame:

```json
{ "id": "c-7f3a", "api_version": 1, "cmd": "mixer.set", "args": { "channel": "live", "gain_db": -6.0 } }
```

| Feld          | Typ    | Pflicht | Bedeutung |
| ------------- | ------ | ------- | --------- |
| `id`          | string | ja      | Client-Korrelations-ID; in der Antwort zurückgegeben |
| `api_version` | number | nein    | Default `1`; abweichend → `version_mismatch` |
| `cmd`         | string | ja      | Command-Name (§6.1.1) |
| `args`        | object | nein    | Command-spezifische Argumente |

Antwort (Erfolg):

```json
{ "id": "c-7f3a", "ok": true, "result": { "applied": true } }
```

Antwort (Fehler):

```json
{ "id": "c-7f3a", "ok": false, "error": { "code": "bad_gain", "message": "gain_db out of range", "details": { "field": "gain_db" } } }
```

Die Bridge antwortet **immer** auf jedes Command mit derselben `id` (oder
`id: null` bei Frames ohne `id`).

#### 6.1.1 Command-Namen (WS)

Die WS-Commands spiegeln §4.1–§4.4, mit `args`-Objekten statt Positionsargumenten:

| `cmd`              | `args`                                              |
| ------------------ | --------------------------------------------------- |
| `mixer.set`        | `{channel, gain_db? , linear?}`                     |
| `mixer.trim`       | `{channel, gain_db}`                                |
| `mixer.mute`       | `{channel, mute}`                                   |
| `mixer.solo`       | `{channel, solo}`                                   |
| `mixer.exclusive`  | `{channel}`                                         |
| `mixer.patch`      | `{channel, source}`                                 |
| `mixer.master`     | `{gain_db}`                                         |
| `layer.preset`     | `{preset}`                                          |
| `layer.force`      | `{layer}`                                           |
| `layer.enable`     | `{layer, enabled}`                                  |
| `monitor.enable`   | `{enabled}`                                         |
| `monitor.status`   | `{}`                                                |
| `live.set`         | `{mount}`                                           |
| `live.clear`       | `{}`                                                |
| `live.status`      | `{}`                                                |
| `tape.start`       | `{tape_id}`                                         |
| `tape.stop`        | `{}`                                                |
| `state.get`        | `{}` → `result` = `CockpitState`                    |
| `ping`             | `{}` → `result` = `{ "pong": true, "t": "…" }`      |

`gain_db` und `linear` schließen sich aus; sind beide gesetzt → `validation_error`.

### 6.2 Bridge → Client (Meter-Frames, binär)

Die Bridge pusht **30 Hz** binäre Meter-Frames. Layout (Little-Endian):

| Offset | Größe | Typ       | Bedeutung |
| ------ | ----- | --------- | --------- |
| 0      | 1     | u8        | Protokollversion = `1` |
| 1      | 1     | u8        | Kanalanzahl `N` (N-generisch, siehe unten) |
| 2      | 2     | u16       | reserviert (`0`) |
| 4      | N×12  | f32[3N]   | pro Kanal: `rms_l`, `rms_r`, `peak` (dBFS) |

- Gesamtlänge = `4 + N*12` Bytes.
- `N` ist die Anzahl der Kanäle in `CockpitState.channels` (nicht mehr fix 6).
  Es gilt das Modell aus §5.5: 6 Program + `harbor.slot-NN` + `relay.slot-NN` +
  `adhoc-<n>`.
- **`N` ist ein `u8`** → **Cap 255 Kanäle**. Das Zielmodell
  (6 Program + bis zu 32 Harbor + 8 Relay + Adhoc) liegt deutlich darunter; wird der
  Cap dennoch überschritten, ist ein Frame-Layout-Bump nötig (separates Issue).
- Kanalreihenfolge = Reihenfolge von `channels[]` im letzten `CockpitState` (§5.5).
- Ändert sich die Reihenfolge/Anzahl, sendet die Bridge vorher ein
  `meters.layout`-Event (§6.3) mit der neuen `channel_ids`-Liste.
- Werte `-inf` werden als `-3.4028235e38` (f32 MIN) kodiert; Clients interpretieren
  das als „kein Signal".
- Clients, die keine Binärframes verstehen, senden `{"cmd":"meters.subscribe","args":{"binary":false}}`
  und erhalten stattdessen `meters`-JSON-Events (§6.3).

### 6.3 Bridge → Client (JSON-Events)

Alle Events haben `type` und `at` (ISO-8601 UTC):

```json
{ "type": "state", "at": "2026-11-22T19:42:10Z", "data": { /* CockpitState */ } }
```

| `type`              | `data`                                             | Auslöser |
| ------------------- | -------------------------------------------------- | -------- |
| `hello`             | `{api_version, server, session_id}`                | Verbindungsaufbau |
| `state`             | `CockpitState` (Voll-Snapshot)                     | nach Auth, danach bei jeder Zustandsänderung |
| `state.diff`        | `{channels?: [...], master?, automation?, …}`      | inkrementelle Änderung |
| `meters.layout`     | `{channel_ids: [...]}`                             | Kanal-`channels[]`-Anzahl **oder** -Reihenfolge geändert (z. B. neuer Adhoc-Kanal, `harbor.slot-NN` verbunden/getrennt) |
| `meters`            | `{frames: [{id, rms_l, rms_r, peak}]}`             | nur bei `binary:false` |
| `event`             | `{kind, …}` (z. B. `layer.changed`, `live.started`) | fachliches Event |
| `error`             | `{code, message, details?}`                        | asynchroner Fehler (z. B. Engine weg) |
| `pong`              | `{t}`                                              | Antwort auf `ping` |

Regeln:

- `hello` ist immer das erste Event nach erfolgreicher Auth.
- `state` folgt direkt nach `hello`; Clients rendern daraus.
- `state.diff` ist additiv; Clients mergen per `channels[].id`.
- Ein `error`-Event beendet die Verbindung **nicht** (außer `engine_unavailable`
  mit Close-Code `4503`).

### 6.4 WS-Fehlercodes (zusätzlich zu §3.1)

| `code`              | Bedeutung |
| ------------------- | --------- |
| `unknown_command`   | `cmd` nicht im Vertrag |
| `invalid_args`      | `args` fehlerhaft/fehlend |
| `channel_not_found` | `channel`/`id` unbekannt |
| `bad_gain`          | Gain außerhalb `[-60, +12]` dB oder linear außerhalb `[0,1]` |
| `bad_source`        | `source`-Präfix ungültig |
| `command_failed`    | Liquidsoap meldete `ERROR …` |
| `engine_unavailable`| Liquidsoap-Command-Server nicht erreichbar |
| `version_mismatch`  | `api_version` unbekannt |
| `rate_limited`      | zu viele Commands (Fader-Drossel) |

### 6.5 Drosselung

Fader-Commands (`mixer.set`, `mixer.trim`, `mixer.master`) dürfen mit bis zu
**50 Hz** gesendet werden; die Bridge kollabiert sie auf den letzten Wert pro
Kanal und wendet höchstens **30 Hz** auf die Engine an. Überschreitet ein Client
100 Commands/s, antwortet die Bridge mit `rate_limited` und verwirft den Rest des
Bursts.

---

## 7. Beispiele

### 7.1 HTTP: State lesen

```http
GET /api/v1/control/state HTTP/1.1
Authorization: Bearer <token>
```

```json
{
  "schema_version": 1,
  "generated_at": "2026-11-22T19:42:10Z",
  "channels": [
    {
      "id": "live", "label": "Live", "kind": "live",
      "source": "harbor:/live/slot-01", "connected": true,
      "gain_db": -6.0, "trim_db": 0.0, "mute": false, "solo": false,
      "exclusive": false, "rms_l": -18.2, "rms_r": -18.0, "peak": -3.1,
      "limiter_active": false, "metadata": { "title": "…", "artist": "…" }
    }
  ],
  "master": { "gain_db": 0.0, "limiter_active": false, "outputs": { "icecast": true, "hls": true, "tape": false } },
  "automation": { "mode": "auto", "preset": "default", "next_event_at": "2026-11-22T20:00:00Z", "next_source": "show", "override": null },
  "monitors": { "connected_djs": 2, "latency_ms": 120, "codec": "opus" },
  "listeners": 42,
  "now_playing": { "title": "…", "artist": "…", "source": "rotation" }
}
```

### 7.2 WS: Fader setzen

```json
→ { "id": "c-1", "api_version": 1, "cmd": "mixer.set", "args": { "channel": "live", "gain_db": -6.0 } }
← { "id": "c-1", "ok": true, "result": { "applied": true } }
```

### 7.3 WS: Zustandsänderung

```json
← { "type": "state.diff", "at": "2026-11-22T19:42:11Z", "data": { "channels": [ { "id": "live", "gain_db": -6.0 } ] } }
```

### 7.4 WS: Meter-Frame dekodieren (Pseudocode)

```text
version = frame[0]          # 1
n       = frame[1]          # Kanalanzahl
for i in 0..n:
    rms_l = f32le(frame, 4 + i*12 + 0)
    rms_r = f32le(frame, 4 + i*12 + 4)
    peak  = f32le(frame, 4 + i*12 + 8)
```

---

## 8. Geltung / Abgrenzung

- **In Scope:** der oben definierte Vertrag. M14-Issues bauen dagegen.
- **Nicht in Scope:** EQ/Filter (`filter.iir.eq.*`) — v2 nicht vorgesehen; WebRTC-
  Signalisierung (separates `monitor-service`-Issue); Persistenz der Kanal-Layouts.
- Änderungen an diesem Dokument sind **vertragsrelevant** und brauchen Review durch
  Engine- und Bridge-Owner.

## 9. Referenzen

- `docs/consolidated-architecture.md` §4.4/§4.6/§6.10.1 (Commands, Fehler, RBAC).
- `docs/product-v2-cockpit.md` §7/§8 (Mixer-Graph, UI-Datenmodell).
- `scheduler/src/liquidsoap.rs` (Command-Server-Client, bestehende Commands).
- `scheduler/src/api/control.rs`, `scheduler/src/dto.rs` (ControlStateDto).
