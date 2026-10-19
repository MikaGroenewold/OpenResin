# SDCP (Smart Device Control Protocol) – implementation notes

OpenResin implements SDCP from the public specification published by CBD
Technology (`SDCP(Smart Device Control Protocol)_V3.0.0`, github.com/cbd-tech).
This page only records what we observed on real hardware in addition to the
specification. Tested device: **ELEGOO Mars 5 Ultra, firmware V1.5.0,
ProtocolVersion V3.0.0**.

## Discovery

* UDP broadcast of the ASCII string `M99999` to port 3000. The printer answers
  from port 3000 with a JSON document (`Id`, `Data.Name`, `Data.MachineName`,
  `Data.BrandName`, `Data.MainboardIP`, `Data.MainboardID`,
  `Data.ProtocolVersion`, `Data.FirmwareVersion`).
* Broadcast on every interface's directed broadcast address as well as
  `255.255.255.255`; some systems only route one of them.
* Unicast `M99999` to a known IP works too (used for "add by IP").

## Control channel

* `ws://<ip>:3030/websocket`, JSON text frames. Requests carry the `Id` from
  discovery, `RequestID` (any unique hex string), `MainboardID`, `TimeStamp`
  in **seconds**, `From` (0 = PC on the LAN) and the `Topic`
  `sdcp/request/<MainboardID>`.
* Responses arrive on `sdcp/response/<MainboardID>` with the same `RequestID`.
  Status (`sdcp/status/…`) is pushed roughly every 1–3 s while printing and on
  every change; `Cmd 0` forces a refresh.
* `CurrentStatus` is an array (`[1]`), not a number, on V3 firmware.
  `CurrentTicks`/`TotalTicks` are milliseconds.
* `Cmd 258` (file list) returns entries with `name` (full path such as
  `/local/file.goo`) and `type` only – no sizes. Listing `/usb` without a USB
  drive returns `Ack: -1`.
* Text `ping` is answered with `pong`.

## File upload (V3)

`POST http://<ip>:3030/uploadFile/upload`, `multipart/form-data`, 1 MiB chunks,
fields `S-File-MD5` (MD5 of the whole file, lowercase hex), `Check` (`1`),
`Offset`, `Uuid` (same for all chunks), `TotalSize`, `File` (the chunk).
Success is `{"success": true}`. OpenResin falls back to the printer-pulls-file
mode (`Cmd 256` with a `URL` served by a short-lived local HTTP server) for
mainboards that report a protocol version below 3.

## Reading stored files

The same web server (port 3030) serves the internal storage read-only below
`/media/mmcblk0p3/`: a file listed as `/local/name.goo` is available at
`http://<ip>:3030/media/mmcblk0p3/name.goo`. HTTP `Range` requests are
honoured, so OpenResin reads only the header of GOO/classic CTB files to show
thumbnails in the file manager, and can download complete files. History
thumbnails (`Cmd 321`) are BMPs under `/media/mmcblk0p1/history_image/`.
The USB mount point has not been confirmed, so downloads are offered for
internal storage only.

## Camera

`Cmd 386 {"Enable": 1}` returns `VideoUrl`, e.g. `rtsp://<ip>:554/video`
(H.264, 1280×720, ~3 fps). The firmware's RTSP server has several quirks that
strict RTSP clients reject; OpenResin's own client handles all of them:

1. Responses contain a malformed header line `Data :<date>` (should be
   `Date: …`). Parse headers leniently.
2. Only RTP over **UDP** works. A TCP-interleaved `SETUP` is answered with a
   UDP transport and a garbled `client_port`.
3. The `Transport` header must be spelled `RTP/AVP/UDP;unicast;client_port=a-b`.
4. Client ports above 32767 silently receive nothing (the port appears to be
   stored as a signed 16-bit value). Use an even/odd port pair below 32768.
5. The payload is not RFC 6184: it is a raw Annex B byte stream (start codes,
   real NAL headers). Small units are sent as plain Annex B packets; larger ones
   are split into "FU-A" packets whose FU header carries NAL type 0. Treat
   fragments with type 0 that start with a start code as raw Annex B.
6. SPS/PPS are not in the SDP; they arrive in-band before each IDR frame.
7. The printer allows `MaximumVideoStreamAllowed` (2) viewers. Viewer slots are
   not always released when a client disconnects and `Enable: 0` does not free
   them either; sending RTSP `TEARDOWN` is the polite way to leave, and a
   printer restart clears stuck slots. The RTSP URL keeps working even when all
   slots are reported as taken.

On Windows the firewall must allow inbound UDP for the application (Windows
asks on first use).
