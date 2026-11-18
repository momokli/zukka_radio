# Liquidsoap

## 1. Scope

Dieses Dokument legt die Audio-Engine des v2-Stacks fest: Script-Struktur,
Quellen, Prioritätenkette, Multi-Caster-Verhalten, Recording, Relay, HLS und die
Control-Schnittstelle.

**Nicht in Scope:** Icecast-Konfiguration, Caddy-Routing, Scheduler-Interna,
Portal-UI. Zu diesen wird hier nur die jeweilige Schnittstelle definiert.

Die Schnittstellen sind auf `data-model.md`, `portal.md` und
`infrastructure.md` abgeglichen. Wo dieses Dokument davon abweicht, steht die
Abweichung explizit dabei (§2 D3, §4).

### 1.1 Verifizierte Grundlagen

Alle folgenden Punkte wurden am 2026-11-18 **lesend** gegen den auf
`cast.zukkafabrik.de` laufenden Liquidsoap-2.4.5-Build geprüft
(`liquidsoap --version`, `liquidsoap -h <op>`, `liquidsoap --list-functions`):

| Prüfung                | Ergebnis                                                                                                                                                                               |
| ---------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Version                | Liquidsoap 2.4.5 (OCaml 4.14.2)                                                                                                                                                        |
| `input.harbor`         | vorhanden; `port`, `user`, `password`, `auth`, `buffer`, `max`, `replay_metadata`, `transport`; Methoden `stop`, `connected_client`, `status`; Callbacks `on_connect`, `on_disconnect` |
| `request.dynamic`      | vorhanden; `available` (Ref), `prefetch`, `synchronous`, `retry_delay`; Methoden `add`, `set_queue`, `queue`, `current`, `fetch`, `skip`                                               |
| `switch`               | vorhanden; `track_sensitive`, `transitions`, `transition_length`, `selected`                                                                                                           |
| `fallback`             | vorhanden; `track_sensitive`, `transitions`, `transition_length`                                                                                                                       |
| `crossfade`            | vorhanden; `duration`, `fade_in`, `fade_out`, `start_duration`, `end_duration`                                                                                                         |
| `playlist`             | vorhanden; `reload`, `reload_mode`, `mode`, `loop`, `prefix`, `cue_in_metadata`, `cue_out_metadata`                                                                                    |
| `output.icecast`       | vorhanden; `mount`, `host`, `port`, `password`, `icy_metadata`, `on_connect`/`on_disconnect`                                                                                           |
| `output.file`          | vorhanden; `start`, `reopen_on_metadata`, `reopen_when`, `append`                                                                                                                      |
| `input.http`           | vorhanden; `max_buffer`, `poll_delay`, `on_error`, `self_sync`                                                                                                                         |
| `server.register`      | vorhanden; `namespace`, `description`, `usage`, `(string) -> string`                                                                                                                   |
| `server.harbor`        | vorhanden; `port` (Default 8000), `uri` (Default `/telnet`)                                                                                                                            |
| `harbor.http.register` | vorhanden; `port`, `method`, `path`, Handler                                                                                                                                           |
| **`output.hls`**       | **existiert nicht** („Plugin not found")                                                                                                                                               |
| **`output.file.hls`**  | **vorhanden**; `segment_duration`, `segments`, `playlist`, `persist_at`, `segment_name`, `[("name", format)]`                                                                          |
| `record`               | **nicht gefunden** — `-h record` und `--list-functions \| grep -i record` liefern nichts; Recording läuft über `output.file`                                                           |
| Encoder                | `lame`, `shine`, `fdkaac`, `ogg`, `flac`, `ffmpeg`, `wav`                                                                                                                              |
| Protokolle             | `file`, `http`, `https`, `annotate`, `replaygain`, `lufs_track_gain`, `autocue`                                                                                                        |

Zwei Konsequenzen daraus, die den Rest des Dokuments prägen:

1. **HLS heißt `output.file.hls`.** Es gibt keinen `output.hls`-Operator. Wer
   nach `output.hls` sucht, findet nichts und schließt fälschlich auf fehlende
   HLS-Unterstützung.
2. **Es gibt keinen `record`-Operator.** Verlustfreies Mitschneiden ist
   `output.file` mit `%flac` auf dem Master vor dem Encoding.

`input.harbor` hat einen `transport`-Parameter. Damit ist die in
`infrastructure.md` offene Frage „terminiert `input.harbor` TLS?" beantwortbar:
Der Operator kann einen `http.transport.ssl`/`secure_transport` verwenden, sofern
der Ziel-Build diesen Transport mitbringt. Ob das der Fall ist, wurde **nicht**
geprüft.

Nicht einzeln verifiziert, aber in den Skizzen dieses Dokuments verwendet:
`http.post`, `json.stringify`, `list.remove`, `list.hd(default=…)`, die
`start()`/`stop()`-Methoden von `output.file` sowie die Cue-Metadaten
`liq_cue_in`/`liq_cue_out`. Für Liquidsoap 2.x plausibel, im ersten
Umsetzungsschritt gegen den Ziel-Build zu prüfen.

> **Wichtig:** Der geprüfte Build ist der des aktuell auf dem Host laufenden
> Fremdsystems. Der neue Stack nutzt laut `infrastructure.md`
> `savonet/liquidsoap@sha256:…` — ein **anderes** Image. Dass `output.file.hls`
> dort vorhanden ist, ist nicht automatisch gegeben und muss im ersten
> Umsetzungsschritt erneut geprüft werden (§6, Schritt 1).

---

## 2. Entscheidungen

### D1 — Image per Digest gepinnt, Version 2.4.5

`savonet/liquidsoap@sha256:…` (so in `infrastructure.md` vorgesehen), Digest
gepinnt, kein Tag. Der Digest muss auf einen Build zeigen, der 2.4.5 entspricht
oder als kompatibel verifiziert ist. Begründung: Audio-Verhalten ändert sich
zwischen Minor-Versionen (Buffer-Semantik, Transition-Defaults); ein
unbeabsichtigtes Upgrade ist ein On-Air-Risiko.

### D2 — Modulares Script, kein Hot-Reload

Liquidsoap kann das Script nicht im Betrieb neu laden. Daraus folgt die
Trennung:

- **Daten** (Caster, Rotation, Queue, Show, Tape-Bereitschaft) kommen zur
  Laufzeit über HTTP und Control-Commands. Änderungen brauchen **keinen**
  Neustart.
- **Struktur** (neue Ebene, neuer Encoder, neue Transition, neuer Caster-Slot)
  braucht einen Neustart. Das ist ein bewusst seltener Vorgang.

Das Script wird deshalb so gebaut, dass es möglichst wenig über die Welt weiß.

### D3 — Caster-Pool aus vorprovisionierten Slots

`input.harbor` bindet seinen Mount statisch an einen String. Quellen können zur
Laufzeit nicht erzeugt werden. Ein Mount pro Caster (wie in `portal.md` mit
`/live/mira` illustriert) würde bei jedem neuen Caster ein Script-Update und
einen Neustart erzwingen.

Deshalb: **fester Pool** vorprovisionierter Slots `/live/slot-01` …
`/live/slot-NN` (Default `N = 32`). `casters.harbor_mount` aus `data-model.md`
bleibt die Reservierung: Beim Anlegen eines Casters wird ihm ein freier Slot
zugewiesen und in dieser Spalte gespeichert. Der Auth-Check akzeptiert nur den
zugewiesenen Mount.

Das ist eine bewusste Abweichung vom Beispiel `/live/<slug>` in `portal.md`.
Grund: Ein Caster, der `/live/mira` erwartet, ist eine Semantik, die einen
Neustart pro Caster-Neuanlage erzwingt. Ein Slot ist die technisch ehrliche
Variante — das Portal zeigt dem Caster seinen Slot als „dein Mount".

Konsequenz: `N` muss **≥ Anzahl aktiver Caster** sein. Eine Pool-Vergrößerung
ist ein Script-Änderung mit Neustart (§4, O1).

### D4 — Auth über das Portal

`input.harbor(auth=...)` bekommt `({address, password, user}) -> bool`. Der
Callback ruft `POST /internal/harbor/auth` auf dem **Portal** auf (Contract in
`portal.md` §3.4), nicht auf dem Scheduler.

- Timeout 500 ms. `portal.md` fordert eine Antwort in **< 200 ms**; Liquidsoap
  weist die Verbindung bei Überschreitung seines Timeouts ab.
- **Fail-closed** bei unbekannten Credentials.
- Etablierte Verbindungen werden nicht nachträglich gekappt — `auth` läuft nur
  beim Verbindungsaufbau. Ein Credential-Entzug wirkt erst beim nächsten
  Connect. Das ist gewollt: „Kein Kick/Bann — nur wer ist live."

### D5 — Der Scheduler ist Autorität, Liquidsoap hält eine Fail-Safe-Reihenfolge

`data-model.md` definiert `POST /live/select` und `POST /live/next` auf dem
Scheduler. Damit ist der Scheduler fachlich zuständig: er kennt Caster, führt
die Warteschlange und entscheidet über Overrides.

Liquidsoap führt **zusätzlich** eine lokale FCFS-Reihenfolge über
`on_connect`/`on_disconnect`. Sie greift nur, solange kein expliziter Override
gesetzt ist. Zweck: Fällt der Scheduler aus, funktionieren laufender Betrieb und
Handoff weiter.

Damit gilt: Scheduler entscheidet, Liquidsoap führt aus — mit einem lokalen
Default, der den Ausfall überbrückt. Nach einem Scheduler-Neustart muss der
Scheduler seinen Zustand erneut an Liquidsoap pushen, weil der Override nur im
Liquidsoap-Prozess lebt.

### D6 — Prioritätenkette mechanisch in Liquidsoap, inhaltlich im Scheduler

Liquidsoap entscheidet „höchste verfügbare Ebene gewinnt" über `fallback`.
Der Scheduler entscheidet, **was** jede Ebene anbietet und **ob** sie verfügbar
ist.

Die Ebenen entsprechen exakt den Seed-Sources aus `data-model.md` §3.1.3:

| Prio | Ebene    | `source_id` | Träger in Liquidsoap                    | Verfügbarkeit kommt von        |
| ---- | -------- | ----------- | --------------------------------------- | ------------------------------ |
| 100  | live     | 1           | `switch` über Caster-Pool               | `on_connect` / `on_disconnect` |
| 80   | show     | 2           | `request.dynamic`                       | Scheduler (`/playback/next`)   |
| 60   | queue    | 3           | `request.dynamic`                       | Scheduler (`/playback/next`)   |
| 40   | rotation | 4           | `playlist` (M3U, `reload_mode="watch"`) | immer verfügbar                |
| 20   | relay    | 5           | `input.http` (SomaFM)                   | Verbindung zum Relay           |
| 1    | fun      | 6           | `playlist(loop=true)`                   | immer verfügbar                |

Die `source_id`-Werte sind die IDs der Seed-Inserts in `data-model.md` und
gelten nur, solange die Seed-Reihenfolge unverändert bleibt. Deshalb werden sie
**zur Laufzeit vom Scheduler aufgelöst** und nicht im Script hartkodiert: Der
Scheduler liefert pro Layer eine ID, Liquidsoap fragt mit dieser ID.

### D7 — Ein `request.dynamic` pro Ebene, nicht pro Caster

Queues sind pro Caster benannt, aber es läuft immer nur eine. Welche, entscheidet
der Scheduler. Liquidsoap fragt `GET /playback/next?source_id=…` und bekommt das
nächste `PlaybackItemDto`. Damit bleibt das Script statisch, egal wie viele
Caster und Queues es gibt.

### D8 — Rotation als beobachtete M3U

`playlist("/tracks/rotation.m3u", reload_mode="watch")`. Der Scheduler schreibt
die Datei atomar (temp + rename). Kein Neustart, kein Control-Call.

### D9 — Relay als `input.http` mit begrenztem Buffer

SomaFM Groove Salad als `input.http` mit `max_buffer` (Default 10 s) und
`on_error`-Retry. Der Relay ist die „die Welt ist noch da"-Ebene: er ist
unabhängig von unserem Storage und unserer Datenbank.

> **Konflikt:** `infrastructure.md` gibt Liquidsoap **keinen Internet-Egress**
> (§2.3) und führt es gleichzeitig auf dem `front`-Netz (§4.1). Ein
> `input.http` auf `ice1.somafm.com` ist ohne Egress unmöglich. Auflösung siehe
> §4, O3 — der Relay braucht entweder eine eng gefasste Egress-Regel oder einen
> eigenen Sidecar.

### D10 — Fun-Layer garantiert, dass der Stream nie stirbt

`playlist(loop=true)` auf einen lokalen Ordner. Diese Ebene ist **immer**
verfügbar. Damit kann `fallback` nie „nichts" liefern und der Icecast-Source
bricht nicht ab, wenn Rotation, Relay und Live gleichzeitig weg sind.

### D11 — Crossfade: Musik ja, Live nur kurz

- Track-zu-Track auf show/queue/rotation: `crossfade` mit Default-Dauer.
- Live-Handoff: kurzer Fade (0,5 s out / 0,5 s in), **kein** voller Crossfade.

Grund: `input.harbor` dokumentiert selbst, dass der Default-`buffer` (12 s)
existiert, damit `crossfade` genug Material hat, und dass ein kleinerer Buffer
die Crossfade-Qualität gefährdet. Ein 12-s-Buffer bedeutet aber ~12 s Latenz
vom Caster zum Hörer — unvereinbar mit „Kein Delay". Wir wählen den Buffer
klein und verzichten auf den langen Crossfade im Live-Pfad.

### D12 — Recording: verlustfrei vor dem Encoding, pro Broadcast eine Datei

Der Tape-Tap sitzt auf dem **Master** (PCM), nicht auf dem encodierten Stream.
Damit ist die Aufnahme unabhängig von der Stream-Bitrate.

`output.file(%flac, ...)` mit `start=false`; der Scheduler startet und stoppt
die Aufnahme an Broadcast-Grenzen. Der Dateiname kommt aus der Tape-ID.
Ziel ist `recordings_staging:/recordings` (Volume laut
`infrastructure.md` §4.1), von wo der Upload in die Music API erfolgt.

### D13 — HLS als Latenz-Fallback, nicht als Primärweg

`output.file.hls` schreibt Segmente in das `hls`-Volume, das Caddy read-only
ausliefert. HLS ist inhärent verzögert (Segmentdauer × Segmentanzahl). Icecast
bleibt der latenzarme Weg; HLS ist für Clients, die kein Icecast können, und
für zeitversetztes Hören.

### D14 — Control-API auf eigenem, nicht veröffentlichten Port

`server.harbor(port=8010, uri="/control")`. Der Port wird **nicht** auf den Host
publiziert und ist nur über das `back`-Netz erreichbar.

> `infrastructure.md` reserviert 12000–12099 und nennt keinen Control-Port. 8010
> ist ein Container-interner Port und kollidiert nicht mit dem dortigen Schema —
> er muss aber in die Compose-/Netzwerkdefinition aufgenommen werden.

### D15 — Events pusht Liquidsoap an zwei Ziele

- Verbindungsereignisse → **Portal** (`/internal/harbor/connected`,
  `/internal/harbor/disconnected`, Contract in `portal.md`).
- Wiedergabeereignisse → **Scheduler** (`/playback/started`, `/playback/ended`,
  `/playback/error`, Contract in `data-model.md`).

Auth: Portal `Authorization: Bearer <INTERNAL_TOKEN>`, Scheduler
`X-Liquidsoap-Token` (beides laut `infrastructure.md` §3).

Hörerzahlen und Mount-Status kommen aus den Icecast-Admin-Stats. Zwei Quellen,
klar getrennt: Liquidsoap weiß, _was läuft_; Icecast weiß, _wer zuhört_.

### D16 — Ports und Pfade

| Zweck                  | Container | Host                                             |
| ---------------------- | --------- | ------------------------------------------------ |
| Icecast                | 8000      | `127.0.0.1:12000` (prod) / `12100` (dev)         |
| Harbor (Caster-Ingest) | 8001      | `0.0.0.0:12001` (prod) / `127.0.0.1:12101` (dev) |
| Control                | 8010      | **nicht publiziert**                             |

| Pfad im Container | Volume               | Modus |
| ----------------- | -------------------- | ----- |
| `/tracks`         | `tracks_cache`       | ro    |
| `/hls`            | `hls`                | rw    |
| `/recordings`     | `recordings_staging` | rw    |
| `/etc/liquidsoap` | Repo `liquidsoap/`   | ro    |

Die Host-Ports 8000/8005 sind auf dem Zielhost dicht belegt; deshalb der
12000er-Block. Das ist kein Designproblem, sondern bereits in
`infrastructure.md` §2.2 gelöst.

---

## 3. Schnittstellen

### 3.1 Script-Struktur

```
liquidsoap/
  radio.liq              # Entrypoint: verdrahtet alles, startet Outputs
  lib/config.liq         # Env, Ports, Pfade, Pool-Größe, Tokens
  lib/events.liq         # HTTP-Push an Portal und Scheduler
  lib/harbor.liq         # Caster-Pool, Auth, FCFS-Registry
  lib/live.liq           # Live-Switch + Override
  lib/layers.liq         # show, queue, rotation, relay, fun
  lib/master.liq         # fallback-Kette + Transitions
  lib/outputs.liq        # icecast, hls, tape
  lib/control.liq        # server.harbor + Commands + Status
```

`radio.liq` enthält nur Includes und die Reihenfolge, in der die Ebenen
zusammengesteckt werden. Alles andere ist in `lib/`. Grund: Die Datei, die man
bei einem Vorfall liest, soll kurz sein.

### 3.2 Prioritätenkette — Code-Skizze

```liquidsoap
# lib/master.liq
# Skizze. Reihenfolge = Priorität. Höchste verfügbare Ebene gewinnt.

master = fallback(
  track_sensitive = false,
  transition_length = 1.0,
  transitions = [live_transition],
  [
    live_src,       # 100
    show_src,       #  80
    queue_src,      #  60
    rotation_src,   #  40
    relay_src,      #  20
    fun_src,        #   1
  ]
)
```

`track_sensitive=false` ist für alle Ebenen außer `fun` richtig: Live soll
sofort übernehmen, nicht erst am Ende des laufenden Tracks. Der Preis ist, dass
ein Ebenenwechsel mitten im Track passiert — deshalb die Transition.

```liquidsoap
# lib/live.liq
# Kurzer Fade statt Crossfade: bei getrenntem Harbor-Input ist der alte
# Source nicht mehr zuverlässig lieferfähig, ein langer Crossfade würde
# ins Leere faden.
def live_transition(old, new) =
  add([fade.out(duration=0.5, old), fade.in(duration=0.5, new)])
end
```

Musik-Ebenen bekommen Track-zu-Track-Crossfade, weil dort beide Seiten Dateien
sind und die Dauer bekannt ist:

```liquidsoap
# lib/layers.liq
rotation_src = crossfade(
  playlist("/tracks/rotation.m3u", reload_mode="watch", mode="randomize")
)
```

### 3.3 Multi-Caster

#### Pool und Auth

```liquidsoap
# lib/harbor.liq
caster_mounts = ["/live/slot-01", "/live/slot-02", ...]   # N Slots, aus Env

def harbor_auth(m) =
  # Der Callback kennt den Mount nicht — er kommt per Closure aus dem Slot.
  fun (r) ->
    try
      resp = http.post(
        "http://portal:8080/internal/harbor/auth",
        json.stringify([("mount", m),
                        ("username", r.user),
                        ("password", r.password)]),
        headers = [("Content-Type", "application/json"),
                   ("Authorization", "Bearer #{internal_token()}")],
        timeout = 0.5
      )
      resp != ""
    catch _ -> false
    end
  end
end
```

Anmerkungen:

- `auth` bekommt laut Signatur nur `{address, password, user}` — **nicht** den
  angefragten Mount. Deshalb ist die Auth-Funktion pro Slot eine Closure
  (`harbor_auth(m)`), und der Mount wird aus dem Slot beigesteuert, nicht aus
  dem Callback-Argument.
- `auth` ist synchron und blockiert den Verbindungsaufbau. 500 ms Timeout, weil
  `portal.md` < 200 ms verlangt; Liquidsoaps eigener Timeout ist großzügiger,
  aber ein hängendes Portal darf keinen Caster unbegrenzt warten lassen.
- Ob `http.post` im Ziel-Build vorhanden ist, wurde **nicht** geprüft.

#### FCFS-Registry

```liquidsoap
# lib/harbor.liq
live_order    = ref([])      # Mounts, älteste Verbindung zuerst
live_override = ref(null())  # string? — Scheduler-/Operator-Entscheidung

def harbor_src(m) =
  s = input.harbor(m, port=8001, auth=harbor_auth(m),
                   buffer=2.0, max=20.0, replay_metadata=true)
  s.on_connect(synchronous=false, fun (_) ->
    live_order := list.add(m, live_order())
    emit_harbor("/internal/harbor/connected", m)
  end)
  s.on_disconnect(synchronous=false, fun () ->
    live_order := list.remove(m, live_order())
    emit_harbor("/internal/harbor/disconnected", m)
  end)
  s
end

casters = list.map(fun (m) -> (m, harbor_src(m)), caster_mounts)
```

`buffer=2.0` ist die Latenz-Entscheidung aus D11. Der Wert ist der
Hauptstellhebel für „Kein Delay" und muss gemessen werden (§7).

#### Auswahl

```liquidsoap
# lib/live.liq
def is_live(m) =
  match live_override() with
    | null -> list.hd(default="", live_order()) == m
    | o    -> o == m
  end
end

live_src = switch(
  track_sensitive = false,
  transition_length = 1.0,
  transitions = [live_transition],
  list.map(fun ((m, s)) -> ((fun () -> is_live(m)), s), casters)
)
```

`switch` nimmt die erste Quelle, deren Prädikat wahr ist. Ohne Override ist das
die älteste Verbindung — FCFS. Mit Override (`live.set`) gewinnt der gesetzte
Mount.

#### Waiting Room

„Waiting Room" ist kein eigener Mechanismus, sondern der Zustand _verbunden,
aber nicht auf Sendung_. Er ergibt sich aus:

- `live_order` in Liquidsoap (Reihenfolge),
- `input.harbor.connected_client()` pro Slot (wer hängt dran),
- dem `/status`-Endpoint (§3.7).

Das Portal zeigt daraus die Warteschlange. Ein Caster im Waiting Room hört
nichts von sich und sieht im Portal „Wartend, Position 2". Der Scheduler kann
über `POST /live/next` den nächsten Caster bestimmen und per `live.set`
durchsetzen.

#### Handoff

| Ereignis                           | Verhalten                                                        |
| ---------------------------------- | ---------------------------------------------------------------- |
| Caster A verbindet, niemand live   | A wird sofort live (0,5 s Fade-in)                               |
| Caster B verbindet, A ist live     | B wartet. **Keine** Verdrängung (FCFS)                           |
| A trennt                           | B wird live (0,5 s Fade)                                         |
| Scheduler `live.set /live/slot-03` | 03 wird live, unabhängig von der Reihenfolge                     |
| Scheduler `live.clear`             | zurück zu FCFS                                                   |
| Letzter Caster trennt              | `live_src` unavailable → `fallback` geht auf show/queue/rotation |

Der Handoff ist nahtlos im Sinne von „kein Stille-Loch", nicht im Sinne von
„unhörbarer Übergang". Ein 0,5-s-Fade ist hörbar, aber es gibt keinen Abbruch.

### 3.4 Recording

```liquidsoap
# lib/outputs.liq
tape_id = ref("")

tape_out = output.file(
  %flac(samplerate=44100, channels=2),
  "/recordings/$(tape_id).flac",
  master,
  start = false
)
```

Ablauf:

1. Scheduler setzt `tape_id` und ruft `tape.start`.
2. `output.file` öffnet `/recordings/<tape_id>.flac` und schreibt PCM als FLAC.
3. Scheduler ruft `tape.stop` am Broadcast-Ende.
4. Ein Uploader schiebt die Datei in die Music API und löscht sie lokal
   (`recordings_staging` ist laut `infrastructure.md` §4.1 genau dafür da).

Warum `start=false` und nicht `reopen_on_metadata`: `reopen_on_metadata` würde
bei jedem Trackwechsel eine neue Datei öffnen. Wir wollen eine Datei pro
Broadcast. Der Start/Stop-Zyklus ist die Broadcast-Grenze.

Warum FLAC und nicht MP3: Die Aufnahme ist das Archiv. Sie wird nie
transkodiert, nur beim Ausliefern. Ein Tape, das aus einem 192-kbit-Stream
entsteht, ist kein Tape.

> **Typ-Konflikt (nicht von hier lösbar):** `data-model.md` führt
> `tapes.id` als `INTEGER`, `tapes.md` als `TEXT PRIMARY KEY` (Slug). Der
> Dateiname braucht einen stabilen, dateisystemtauglichen Wert. Vorschlag:
> Slug verwenden, ID nur intern. Siehe §4, O2.

### 3.5 Relay

```liquidsoap
# lib/layers.liq
relay_src = input.http(
  "https://ice1.somafm.com/groovesalad-128-mp3",
  max_buffer = 10.0,
  self_sync = true
)
```

`self_sync=true` ist hier richtig: Der Relay bringt seinen eigenen Takt mit.
Das ist der einzige Punkt in der Kette, an dem eine externe Clock die Quelle
treibt. Sobald eine höhere Ebene übernimmt, wird der Relay nicht mehr
konsumiert.

Ohne Egress-Regel schlägt `input.http` fehl und `on_error` muss den Retry
steuern (§4, O3).

### 3.6 HLS

```liquidsoap
# lib/outputs.liq
output.file.hls(
  playlist = "index.m3u8",
  segment_duration = 4.0,
  segments = 6,
  persist_at = "/hls/.persist",
  "/hls",
  [("aac", %ffmpeg(format="mpegts", %audio(codec="aac", b="128k")))],
  master
)
```

- `segment_duration=4.0`, `segments=6` → ~24 s Fenster, ~8–12 s Latenz.
- `persist_at` verhindert, dass ein Neustart die Segmentzählung neu beginnt und
  Clients mit einem kaputten Playlist-Zustand zurücklässt.
- Der Encoder ist `%ffmpeg` mit `mpegts`-Muxing, weil HLS ein
  Transport-Stream-Container braucht. `%fdkaac` allein liefert ADTS und ist
  nicht direkt als HLS-Segment verwendbar.

> Die exakte Format-Syntax (`%ffmpeg(format="mpegts", ...)`) ist im
> verifizierten Build **nicht** geprüft worden. Sie ist der wahrscheinlichste
> Punkt, an dem die Skizze beim ersten Lauf korrigiert werden muss.

### 3.7 Control-API

```liquidsoap
# lib/control.liq
server.harbor(port = 8010, uri = "/control")
```

Commands via `server.register`:

| Command           | Argument                    | Wirkung                                            |
| ----------------- | --------------------------- | -------------------------------------------------- |
| `live.set`        | `<mount>`                   | Override auf diesen Mount; sofort live             |
| `live.clear`      | —                           | Override aufheben, zurück zu FCFS                  |
| `live.status`     | —                           | JSON: `live_order`, `override`, aktueller Mount    |
| `layer.available` | `<source_id> <true\|false>` | Verfügbarkeit einer Content-Ebene                  |
| `layer.source_id` | `<layer> <id>`              | löst die Seed-`source_id` zur Laufzeit auf         |
| `tape.start`      | `<tape_id>`                 | setzt `tape_id`, startet FLAC-Recorder             |
| `tape.stop`       | —                           | stoppt Recorder                                    |
| `status`          | —                           | JSON: alle Ebenen, aktueller Track, Buffer, Uptime |
| `health`          | —                           | JSON: Checks (§3.7.1)                              |

```liquidsoap
server.register("live.set", description = "Live-Mount erzwingen",
  usage = "<mount>", fun (arg) -> live_override := arg; "ok" end)

server.register("live.clear", description = "Override aufheben",
  fun (_) -> live_override := null(); "ok" end)

server.register("tape.start", description = "Tape starten",
  usage = "<tape_id>", fun (arg) -> tape_id := arg; tape_out.start(); "ok" end)

server.register("tape.stop", description = "Tape stoppen",
  fun (_) -> tape_out.stop(); "ok" end)
```

> Ob `server.harbor` GET-Requests mit Query-Argumenten direkt auf Commands
> mappt, oder ob der Command-String als Body gesendet werden muss, ist im
> ersten Schritt zu verifizieren. Fallback: `harbor.http.register` auf einem
> eigenen Port mit expliziten Handlern.

#### 3.7.1 Health

`health` liefert JSON mit Einzelchecks, damit das Control Panel nicht raten
muss:

```json
{
  "ok": true,
  "checks": {
    "harbor_listening": true,
    "master_ready": true,
    "icecast_connected": true,
    "hls_writing": true,
    "relay_connected": true
  },
  "layer": "live",
  "mount": "/live/slot-02",
  "uptime_s": 84123
}
```

`ok` ist `false`, sobald `master_ready` oder `icecast_connected` false ist.
Alles andere ist degradiert, aber sendbar.

### 3.8 Scheduler → Liquidsoap

Zwei Wege, klar getrennt.

**Pull (Content):** Liquidsoap fragt den Scheduler.

```liquidsoap
# lib/layers.liq
show_available  = ref(false)
queue_available = ref(false)

def next_for(source_id) =
  try
    body = http.get(
      "http://scheduler:8080/api/v1/playback/next?source_id=#{source_id}",
      headers = [("X-Liquidsoap-Token", liquidsoap_token())],
      timeout = 5.0
    )
    if body == "" then null() else request.create(uri_of(body)) end
  catch _ -> null()
  end
end

show_src  = request.dynamic(fun () -> next_for(show_source_id()),  available = show_available,  prefetch = 1)
queue_src = request.dynamic(fun () -> next_for(queue_source_id()), available = queue_available, prefetch = 1)
```

Der Endpoint liefert `200` mit `PlaybackItemDto` oder `204` („nichts da").
Leer bedeutet: Liquidsoap liefert `null()`, die Quelle wird nicht verfügbar,
`fallback` geht eine Ebene tiefer.

Aus dem DTO werden drei Dinge übernommen:

- `uri` → `request.create(...)`.
- `cue_in_ms` / `cue_out_ms` → als `liq_cue_in` / `liq_cue_out`-Metadata, damit
  Liquidsoap die Cue-Punkte respektiert.
- `queue_item_id` → als `liq_queue_item_id`-Metadata, damit die
  `started`/`ended`-Events eindeutig zuordenbar sind (ein Medium kann mehrfach
  in einer Queue stehen, siehe `data-model.md` §3.4).

`prefetch=1` hält genau einen Track vor. Mehr wäre bei einer Queue, die der
Caster live umsortiert, falsch: der vorgehaltene Track wäre schon veraltet.

> **Offener Interface-Punkt:** `data-model.md` zeigt `uri` als
> `https://music-api.example/stream/tracks/42`, `infrastructure.md` gibt
> Liquidsoap aber **keinen Internet-Egress** und einen `tracks_cache` (ro). Beide
> Angaben sind nicht gleichzeitig erfüllbar. Entweder liefert der Scheduler
> einen lokalen Pfad in `/tracks`, oder Liquidsoap bekommt eine Egress-Regel zur
> Music API. Siehe §4, O4.

**Push (Control):** Der Scheduler setzt Zustand.

```
GET http://liquidsoap:8010/control/live.set?/live/slot-03
GET http://liquidsoap:8010/control/live.clear
GET http://liquidsoap:8010/control/layer.available?2%20false
GET http://liquidsoap:8010/control/tape.start?bc-2026-11-18-2000
```

Der Scheduler erreicht Liquidsoap über das `back`-Netz. Der Port ist nicht
publiziert (§2 D14).

### 3.9 Liquidsoap → Portal und Scheduler

```liquidsoap
# lib/events.liq
def emit(url, body, token_header) =
  ignore(http.post(url, json.stringify(body), headers = token_header))
end

def emit_harbor(path, mount) =
  emit("http://portal:8080" ^ path,
       [("mount", mount), ("ts", string_of(time()))],
       [("Content-Type", "application/json"),
        ("Authorization", "Bearer #{internal_token()}")])
end

def emit_playback(kind, item) =
  emit("http://scheduler:8080/api/v1/playback/" ^ kind,
       item,
       [("Content-Type", "application/json"),
        ("X-Liquidsoap-Token", liquidsoap_token())])
end
```

Events:

| Event          | Ziel      | Auslöser                                 |
| -------------- | --------- | ---------------------------------------- |
| `connected`    | Portal    | `on_connect` eines Harbor-Slots          |
| `disconnected` | Portal    | `on_disconnect`                          |
| `started`      | Scheduler | `on_track` des Master                    |
| `ended`        | Scheduler | `on_track`-Wechsel                       |
| `error`        | Scheduler | fehlgeschlagener Request / Output-Fehler |

Der Scheduler darf diese Events **nicht** als Steuerungssignal verwenden — sie
sind Beobachtung. Steuerung läuft ausschließlich über Control-Commands. Sonst
entsteht eine Rückkopplung, die bei Paketverlust oszilliert.

Push darf den Audio-Pfad nie blockieren: alle Events laufen mit
`synchronous=false` und einem eigenen Timeout.

---

## 4. Offene Fragen

**O1 — Pool-Größe vs. Caster-Anzahl.**
`N=32` Slots, aber `N` muss ≥ Anzahl aktiver Caster sein. Bei Caster 33 ist ein
Script-Change mit Neustart fällig. Optionen: `N` großzügig wählen (Kosten pro
idle `input.harbor` ungemessen), oder das Modell auf „Mount pro Caster" umstellen
und Neustarts bei Caster-Anlage akzeptieren. Muss mit dem Datenmodell-Owner
entschieden werden — `data-model.md` hat `harbor_mount UNIQUE` pro Caster, was
beide Lesarten zulässt.

**O2 — Tape-ID: INTEGER oder TEXT.**
`data-model.md` (`tapes.id INTEGER`) und `tapes.md` (`tape_id TEXT PRIMARY KEY`,
Slug) widersprechen sich. Der Dateiname braucht etwas Stabiles und
Dateisystemtaugliches. Vorschlag: Slug. Entscheidung liegt außerhalb dieses
Dokuments, blockiert aber `tape.start`.

**O3 — Relay ohne Egress.**
`infrastructure.md` verbietet Liquidsoap Internet-Egress und führt es
gleichzeitig auf `front`. Der SomaFM-Relay braucht eine der beiden Auflösungen:
(a) eng gefasste Egress-Regel nur für `ice*.somafm.com`, (b) eigener
Relay-Sidecar mit Egress, der intern unter `http://relay:8000/stream`
ausliefert. (b) erhält das Prinzip „Audio-Container ohne Egress" und ist die
sauberere Variante, kostet aber einen Container.

**O4 — Ist `PlaybackItemDto.uri` lokal oder remote?**
Siehe §3.8. Muss zwischen Scheduler und Infrastructure entschieden werden,
bevor `request.dynamic` implementiert wird.

**O5 — Buffer vs. Crossfade vs. Latenz.**
`buffer=2.0` ist eine Schätzung. Zu klein → Handoff knackt, zu groß → Latenz.
Zu messen: Caster-Mikro → Hörer-Ohr bei 2,0 / 3,0 / 5,0 s Buffer. Entscheidung
fällt nach Messung, nicht vorher.

**O6 — HLS-Format.**
`%ffmpeg(format="mpegts", %audio(codec="aac", b="128k"))` ist ungeprüft.
Alternativen: `%fdkaac` mit explizitem TS-Muxing, oder nur `%mp3` in HLS
(kompatibler, schlechter). Muss im ersten Lauf verifiziert werden.

**O7 — Verhalten bei erschöpftem Pool.**
„Alle Slots belegt" braucht eine definierte Antwort: ablehnen (klar) oder
LRU-Slot recyceln (gefährlich, weil es einen laufenden Caster trennt).
Vorschlag: ablehnen, mit klarer Meldung im Portal. Bestätigung offen.

**O8 — Queue-Semantik.**
„Queue pro Caster, benannt, läuft kontinuierlich im Hintergrund" ist mit
„genau eine Queue ist auf Sendung" nicht vollständig beschrieben. Was passiert
mit der Queue von Caster A, wenn Caster B live geht? Läuft A weiter im
Hintergrund (dann braucht es pro Queue einen eigenen `request.dynamic`, was D7
widerspricht) oder pausiert sie? Produktentscheidung.

**O9 — Auth-Transport.**
`portal.md` definiert `POST /internal/harbor/auth` mit JSON-Body. Ob
Liquidsoaps `http.post` das im Ziel-Build zuverlässig kann, ist ungeprüft.

**O10 — Metadaten-Charset.**
`input.harbor` hat `metadata_charset` und `icy_metadata_charset`. Caster-Tools
senden unterschiedlich (UTF-8 vs. ISO-8859-1). Muss mit echten Clients getestet
werden, bevor ein Default festgelegt wird.

**O11 — TLS für den Harbor-Ingest.**
`infrastructure.md` nennt das als offene Frage. `input.harbor` hat einen
`transport`-Parameter; ob der Ziel-Build einen TLS-Transport mitbringt, ist
ungeprüft. Ohne TLS bleibt Roh-TCP mit starken Passwörtern und Rate-Limit.

**O12 — Scheduler-Neustart re-asserted den Live-Zustand.**
Der Override lebt nur im Liquidsoap-Prozess. Nach einem Scheduler-Neustart muss
der Scheduler `live.set` erneut senden, sonst gilt wieder FCFS und die Anzeige
im Portal weicht ab. Wer das Re-Assert auslöst (Scheduler beim Start, oder
Portal beim ersten Poll), ist nicht entschieden.

---

## 5. Risiken

**R1 — Kein Hot-Reload → Deploy-Lücke.**
Jede Script-Änderung ist ein Neustart und damit eine Unterbrechung des
Icecast-Sources. Größenordnung: 2–5 s Stille, so auch in
`infrastructure.md` §2.9 für `liquidsoap` veranschlagt. Minderung:
Script-Änderungen selten machen (D2), `liquidsoap --check` in CI, Deploy nur im
leeren Fenster. Keine echte Lösung — Liquidsoap kann es nicht.

**R2 — Buffer-Latenz ist ein Produktversprechen.**
„Kein Delay" ist absolut formuliert. Realistisch sind 3–5 s Caster→Hörer
(Harbor-Buffer + Icecast-Puffer + Client-Puffer). Wenn 5 s nicht akzeptabel
sind, muss die Architektur-Anforderung angepasst werden, nicht nur der Buffer.

**R3 — Crossfade auf Live-Quellen ist fragil.**
Ein getrennter `input.harbor` liefert nur noch seinen Restbuffer. Ein langer
Crossfade fadet dann in Stille. Deshalb D11 (kurzer Fade). Wenn der Fade zu
kurz ist, knackt es; wenn er zu lang ist, entsteht ein Loch. Der sweet spot ist
empirisch.

**R4 — HLS-Verfügbarkeit im neuen Image.**
Der verifizierte Build hat `output.file.hls`. Das neue Image ist ein anderes
(`savonet/liquidsoap`, per Digest). Fehlt dort das Plugin, ist D13 blockiert.
Muss vor dem ersten Deployment geprüft werden.

**R5 — Port- und Netz-Kollision beim Parallelbetrieb.**
8000/8005 sind auf dem Host belegt; `infrastructure.md` löst das über den
12000er-Block. Der Control-Port 8010 fehlt in dieser Planung und muss in die
Compose-Definition. Wird er versehentlich publiziert, ist die Live-Steuerung
offen.

**R6 — Auth blockiert den Harbor-Thread und hängt am Portal.**
`auth` ist synchron mit 500 ms Timeout. Ist das Portal down, können sich keine
Caster verbinden (fail-closed). Das ist die Kehrseite von D4. Abwägung: Ein
fail-open würde bei Portal-Ausfall beliebige Credentials akzeptieren — für ein
Radio mit öffentlichem Ingest ist das schlechter.

**R7 — `request.dynamic`-Fehler erzeugen Stille.**
Wenn der Scheduler 5xx liefert und die Rotation ebenfalls leer ist, fällt die
Kette auf Relay und Fun. Das funktioniert nur, solange D10 gilt. Wird der
Fun-Layer jemals entfernt, ist die Kette nicht mehr garantiert geschlossen.

**R8 — FLAC-Platzbedarf.**
44,1 kHz / 16 Bit / Stereo sind ~635 MB/h unkomprimiert; FLAC landet
erfahrungsgemäß bei ~50–60 % davon, also ~0,3–0,4 GB/h. Bei mehreren
gleichzeitigen Broadcasts und verzögertem Upload ist das ein Kapazitätsthema.
Schätzung, keine Messung.

**R9 — Ein Prozess ist ein Single Point of Failure.**
Stürzt Liquidsoap ab, ist der Stream weg, bis der Container neu startet. Ein
zweiter Prozess als Hot-Standby wäre möglich, verdoppelt aber die
Zustandskomplexität (wer hält FCFS und Override?). Bewusst nicht vorgesehen.

**R10 — Icecast-KH vs. Stock-Icecast.**
Der aktuell auf dem Host laufende Icecast ist ein KH-Build
(`2.4.0-kh22`). Der neue Stack sollte Stock-Icecast verwenden, sonst schleppen
wir Verhalten mit, das wir nicht dokumentiert haben. Betrifft
`output.icecast`-Parameter (`send_icy_metadata`, `public`) und die
Admin-Stats.

**R11 — Zwei Event-Ziele, zwei Tokens.**
Liquidsoap schreibt an Portal und Scheduler mit unterschiedlichen
Auth-Schemata. Fällt eines aus, darf das andere nicht blockieren. Jeder Push
braucht einen eigenen kurzen Timeout und muss Fehler schlucken.

---

## 6. Umsetzungsschritte

**1 — Image und Fähigkeiten verifizieren.**
Ziel-Image starten, dann:

```
liquidsoap --version
liquidsoap -h output.file.hls
liquidsoap -h input.harbor
liquidsoap --list-functions | grep -i hls
liquidsoap --list-functions | grep -i "http.post"
```

Abbruchkriterium: `output.file.hls` fehlt → anderes Image oder HLS aus D13
streichen.

**2 — Skelett mit einer Datei.**
`radio.liq` mit `playlist` → `output.icecast`. Nur um zu beweisen, dass
Container, Icecast, Volumes und Caddy zusammen funktionieren. Kein Harbor, kein
Scheduler.

**3 — Harbor-Pool.**
`lib/harbor.liq` mit 2 Slots auf Port 8001, `auth` vorerst statisch (feste
Test-Credentials). Test: zwei Caster parallel verbinden,
`connected_client()` prüfen.

**4 — FCFS + Live-Switch.**
`lib/live.liq`. Test: A verbindet → live. B verbindet → wartet. A trennt → B
live. Override per `server.register` von Hand setzen.

**5 — Control-API.**
`server.harbor` + alle Commands aus §3.7. Test mit `curl` gegen Port 8010 aus
dem `back`-Netz. Dabei O9 und die `server.harbor`-Request-Form klären.

**6 — Content-Ebenen.**
`request.dynamic` gegen einen Stub, der ein `PlaybackItemDto` liefert.
`available`-Refs per Control schalten. Prüfen, dass `fallback` korrekt eine
Ebene tiefer geht und dass `cue_in`/`cue_out` greifen.

**7 — Rotation und Relay.**
M3U schreiben, `reload_mode="watch"` testen (Datei ändern, kein Neustart).
Relay verbinden; ohne Egress-Regel muss `on_error` sauber retryen, ohne den
Master zu blockieren.

**8 — Recording.**
`output.file(%flac)` mit `start=false` nach `/recordings`. Test:
`tape.start`, 30 s laufen, `tape.stop`, Datei mit `flac -t` prüfen. Danach
`ffprobe` gegen den Icecast-Stream: Sample-Rate und Kanäle müssen identisch
sein.

**9 — HLS.**
`output.file.hls` nach `/hls` aktivieren, Segmente zählen, Playlist mit
`ffprobe` abspielen. Latenz gegen Icecast messen (O6).

**10 — Events an Portal und Scheduler.**
`emit_harbor`/`emit_playback` gegen Stubs. Prüfen, dass connect/disconnect in
der richtigen Reihenfolge ankommen und dass ein nicht erreichbares Ziel den
Audio-Pfad **nicht** blockiert.

**11 — Health und Status.**
`status` und `health` implementieren, JSON gegen das im Control Panel
erwartete Schema prüfen.

**12 — Latenz- und Lastmessung.**
O5 messen. Zusätzlich: 4 Caster parallel, einer live, drei wartend; CPU und
Buffer-Verhalten beobachten.

**13 — Deploy-Pfad.**
Compose-Service gemäß `infrastructure.md`, `liquidsoap --check` als
Pre-Deploy-Gate, Neustart nur im leeren Fenster (R1).

---

## 7. Akzeptanzkriterien

**Funktional**

- [ ] Zwei Caster sind gleichzeitig über getrennte Mounts auf Port 8001
      verbunden; genau einer ist auf Sendung.
- [ ] Der zuerst verbundene Caster ist live; ein später verbundener verdrängt
      ihn nicht.
- [ ] Trennt der live Caster, übernimmt der nächste Wartende ohne Stille-Loch.
- [ ] `live.set` schaltet sofort auf den angegebenen Mount, auch gegen die
      FCFS-Reihenfolge; `live.clear` stellt FCFS wieder her.
- [ ] Ein Scheduler-Ausfall ändert den laufenden Live-Betrieb nicht.
- [ ] Trennen alle Caster, läuft die Show-, sonst die Queue-, sonst die
      Rotations-, sonst die Relay-, sonst die Fun-Ebene.
- [ ] Eine geänderte `rotation.m3u` wirkt ohne Neustart.
- [ ] Ein nicht erreichbarer Scheduler führt zu Fallback, nicht zu Stille.

**Recording**

- [ ] `tape.start` erzeugt genau eine FLAC-Datei pro Broadcast.
- [ ] Die Aufnahme ist bit-identisch zum Master (Sample-Rate, Kanäle,
      keine Encoder-Artefakte) — geprüft per `ffprobe`/`flac -t`.
- [ ] `tape.stop` schließt die Datei sauber (kein abgeschnittenes FLAC-Header).

**HLS**

- [ ] `index.m3u8` und Segmente werden in `/hls` geschrieben und von Caddy
      ausgeliefert.
- [ ] Ein Standard-HLS-Client spielt den Stream ab.
- [ ] Ein Neustart von Liquidsoap macht die Playlist nicht dauerhaft kaputt
      (`persist_at` wirkt).

**Control / Health**

- [ ] Alle Commands aus §3.7 sind über Port 8010 aus dem `back`-Netz
      erreichbar.
- [ ] Port 8010 ist vom Host und von außen **nicht** erreichbar.
- [ ] `health` liefert `ok=false`, wenn Icecast nicht verbunden ist.
- [ ] `status` listet verbundene Mounts in FCFS-Reihenfolge.

**Latenz**

- [ ] Caster-Mikro → Hörer-Ohr ≤ 5 s bei laufendem Live-Betrieb (gemessen,
      nicht geschätzt).
- [ ] Der Handoff ist hörbar, aber ohne Unterbrechung (kein Dropout > 100 ms).

**Betrieb**

- [ ] `liquidsoap --check radio.liq` läuft in CI grün.
- [ ] Ein Neustart stellt den Icecast-Source in < 5 s wieder her.
- [ ] 4 parallele Caster verursachen keine Buffer-Warnungen im Log.
- [ ] Liquidsoap hat keinen Internet-Egress — außer dem explizit für den Relay
      freigegebenen Ziel (O3).
