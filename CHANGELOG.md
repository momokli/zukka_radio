# Changelog

Alle nennenswerten Änderungen an diesem Projekt. Format angelehnt an
[Keep a Changelog](https://keepachangelog.com/de/1.1.0/), Versionierung nach
SemVer.

## [Unreleased]

### Added

- Portal: minimale Control-Debug-UI unter `GET /control/debug` (#149). Rendert
  N Kanäle (Fader, Mute, Solo, Meter) ausschließlich über die WebSocket-Bridge
  `/api/v1/control/ws` — kein SSE, kein Polling, kein REST-Read. Platzhalter,
  der später durch das echte Cockpit ersetzt wird.
