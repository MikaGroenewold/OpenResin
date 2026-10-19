# Printer network protocols

| Protocol | Printers | Status in OpenResin |
| --- | --- | --- |
| [SDCP V3](sdcp.md) (WebSocket + HTTP upload, RTSP camera) | ELEGOO Saturn 3/4 Ultra, Mars 4/5 Ultra and other CBD SDCP boards | Discovery, status, file manager (list, upload, download, thumbnails, delete), start/pause/resume/stop, history, time-lapse, rename, camera – tested on a Mars 5 Ultra |
| SDCP < V3 (printer downloads the file, `Cmd 256`) | early Saturn 3 Ultra / Mars 4 Ultra firmware | implemented, untested |
| Legacy Chitu WiFi (G-code over UDP port 3000) | many CTB printers with Chitu WiFi boards | experimental (from public community documentation, not hardware-tested): status, file list, upload (`M28`/chunks/`M29`), start (`M6030`), pause/resume/stop, delete |
| Creality HALOT (WebSocket on port 18188) | HALOT series | not supported yet |
| Anycubic LAN | Photon Mono X/M3 series with WiFi | not supported yet |

Everything is local: OpenResin never talks to vendor clouds, never logs in,
never phones home and has no telemetry or update check.
