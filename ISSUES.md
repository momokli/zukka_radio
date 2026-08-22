# radio zukka — Issues & Fix-Backlog

> Erfasst: 2026-08-22 · Quelle: Live-Diagnose auf `planet` (65.21.27.234) + Repo-Analyse.
> 🔴 P0 kritisch · 🟠 P1 wichtig · 🟡 P2 Robustheit/Cleanup

## 🔴 #1 — ON/OFF-Notifications ausgefallen (live_connect / live_disconnect)

**Symptom:** Im Telegram-Channel `@zukka_radio` kommen keine 🔴/⚫-Meldungen mehr, nur noch 📼 „recording fertig". Ausgefallen seit ~08.08.2026.

**Root cause (bestätigt 2026-08-22):** Der AzuraCast-Container erreicht den Host-Port `5099` nicht mehr.

- AzuraCast-Webhook `zukka pipeline` (id 3, generic, `is_enabled: true`, triggers `live_connect` + `live_disconnect`) → URL `http://172.24.0.1:5099/webhook`.
- Host-seitig läuft `pipeline.py` und lauscht korrekt: `curl 127.0.0.1:5099` → HTTP 400 in ~40 ms.
- Container-seitig schlägt die Verbindung fehl: `docker exec azuracast curl 172.24.0.1:5099` → `http_code=000`, Timeout nach 5 s.
- Docker-Gateway stimmt weiterhin: `azuracast_default` → `gw=172.24.0.1` (Container-IP `172.24.0.3`).
- Letzter erfolgreicher Dispatch: ~08.08. (webhook `last_message_sent=1786235619`); DJs am 09./17./19./21.08. gingen ohne Meldung live.

**Root cause (bestätigt):** `ufw` ist aktiv mit `Default: deny (incoming) … deny (routed)`. Container→Host-Traffic (172.24.0.3 → 172.24.0.1:5099) läuft als INPUT auf dem Bridge-Interface `br-5232f736667c` und wird mangels ALLOW-Regel gedroppt. `ip_forward` ist an (1), Docker-Gateway stimmt — es ist reine ufw-Policy. (ufw seit ~08.08. aktiv → deckt sich mit dem Ausfallzeitpunkt.)

**Quick-Fix (sofort):**
```bash
ufw allow from 172.24.0.0/16 to any port 5099 proto tcp
```

**Dauerhafte Fix-Kandidaten:**
1. Firewall-Regel als dokumentierte Config (Ansible/Repo) statt ad-hoc `ufw allow`.
2. Oder: `pipeline.py` in ein Docker-Netz ziehen, das vom AzuraCast-Container erreichbar ist.
3. Oder: Webhook-Delivery umstellen (native Telegram-Webhooks als Fallback aktivieren).

## 🟠 #2 — Fehlende Observability / kein Alerting

`journalctl -u zukka-webhook` → „No entries", obwohl Service `active/running` (läuft seit 01.08.). Der Ausfall (#1) blieb dadurch tagelang unbemerkt.

- Logging von `pipeline.py` sicherstellen (stdout → journal prüfen).
- Healthcheck + Alarm bei Webhook-Ausfällen (Prometheus/Alertmanager vorhanden: `mellon-alertmanager-telegram`).

## 🟡 #3 — Robustheit & Cleanup

- [ ] `HTTPServer` (single-threaded) → `ThreadingHTTPServer` (ein hängender Request blockiert aktuell alle weiteren).
- [ ] Secrets aus systemd-Units raus (API-Key, Telegram-Token, Funkwhale-PW inline) → `EnvironmentFile` / `.env`.
- [ ] `recording_watcher.py`: Funkwhale-Passwort hardcoded (Base64 `FW_AUTH`) → env.
- [ ] Webhook `timeout: 5` ohne Retry — robuste Zustellung prüfen.
- [ ] Obsolete Scripts in `/srv/azuracast/` entfernen: `webhook_listener.py`, `telegram_notifier.py`, `auto_upload.py`, `app.py`.
- [ ] Deploy: manuelles `scp` (AGENT.md) → CI/Deploy-Hook.
- [ ] 2 tote native Telegram-Webhooks bereinigen oder als Fallback aktivieren (id 1 → `-1003560012041`, id 2 → `@zukka_radio`).

## ℹ️ Kein Issue (Korrektur)

Recording-Pipeline ist **funktionsfähig**: `recording_state.json` enthält 15 Uploads, neuester 21.08. („ANKD — 21.08.26 21:35"). 📼-Notifications kommen weiter.
