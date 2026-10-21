# The application: `recamerad`

One Rust daemon owns the media hardware and serves everything: YOLO detection on the
NPU, arrival/departure events, the web page, MJPEG, RTSP, WebRTC, MQTT with Home Assistant
discovery, an optional LLM/VLM call per arrival, and a terminal in the browser. Small
`recamera-*` programs sit outside the video path. Everything that is site-specific (keys,
prompts, research logic) lives in the camera's configuration or in separate programs that
consume the event stream.

Hardware configuration measured in `docs/multimedia.md`: sensor **1280×960** (2×2
binned), VI→VPSS **online**, 4 blocks per VPSS pool, ION 36 MiB; ch0 640×640 RGB packed
with letterbox for YOLO (handed to the NPU by physical address, no copy); ch1 1280×720
NV21 (the central 16:9 band, no scaling) for H.264 and JPEG.

## 1. Code layout

```
app/                        Cargo workspace, MIT OR Apache-2.0
  sophgo-sys/               hand-written FFI: recamera-cam and the few MPI/cviruntime
                            functions used, with their C structs
  sophgo/                   safe layer, RAII (Sys > Camera > VpssGroup > Frame; nn::Model,
                            yolo, venc::Encoder), teardown in Drop
  recamera-detect/          pure Rust: view mapping, ROI filter, presence state machine (host tests)
  recamera-rtsp/            pure Rust: Annex B, RTP (RFC 6184), SDP, RTSP parsing (host tests)
  recamerad/                the daemon (web page in recamerad/web/, built into the binary)
  recamera-events/          prints the event socket (BusyBox nc has no Unix sockets)
  recamera-yolo/            runs a model over saved frames, to compare with the C reference
external/package/sophgo-mpi/recamera-cam.{c,h}   camera bring-up in C (librecamera-cam.a)
external/package/recamera-app/                   the Buildroot package
```

**Why hand-written FFI, not `bindgen`:** the subset is small and stable, it needs no
libclang in the container, and every struct that crosses the boundary is explicit in Rust.
Sizes and offsets are checked at compile time on both sides (`const` asserts in
`sophgo-sys`, `_Static_assert` in `recamera-cam.c`, the same numbers, measured with the
cross compiler).

**Why the sensor/VI bring-up stays in C:** the Sophgo path (`sample_common_*`) has a sensor
object with callbacks, MIPI, the ISP with its 3A libraries and the tuning binary — dozens
of large structs. Copying them by hand would be the riskiest part of the FFI, for no gain
(it runs once). The frame path (VPSS, VENC, TPU) is Rust over the MPI. The C interface is
small: `rc_reclaim`, `rc_sys_open/close`, `rc_cam_open/close`, `rc_venc_*`, `rc_isp_image`,
`rc_cam_flip`.

**Concurrency:** native threads for the media pipeline (the MPI calls block: capture, TPU,
encoders), and **one `tokio` `current_thread` runtime** for all networking (HTTP, RTSP,
WebRTC, MQTT, WebSocket, LLM). Media and network meet in `Live`: `watch` channels for the
newest JPEG and the newest detections (memory stays flat whatever the number of viewers)
and a short `broadcast` of H.264 access units. Each encoder runs only while someone holds a
viewer guard of it.

**Size and memory:** the release profile is `opt-level = "s"`, LTO, one codegen unit,
`panic = "abort"`, stripped. The daemon is ~4.5 MB with WebRTC, MQTT, the LLM client and
the terminal; RSS 7–17 MB depending on the open sessions. Every build counts `fence.tso` in
the binaries (0). LLVM only emits `fence.tso` for `fence(Ordering::AcqRel)`; any that
appears is emulated by the firmware anyway.

## 2. What it does

| Feature | Details |
|---|---|
| Configuration | `/data/recamera/config.json`: defaults, validation, atomic write (temp file, fsync, rename, fsync dir), `deny_unknown_fields`; `SIGHUP` or the page reloads it, applying what can change live and restarting only what must |
| Logging | syslog (stderr with `-f`); `-v` for debug |
| Events | JSON lines with `type` and `ts`, on stdout (`--events`) and the Unix socket `/run/recamerad/events.sock`; `recamera-events` prints them |
| Telemetry | SoC temperature, load, memory, daemon RSS, every `telemetry.interval_s` |
| Models | any YOLOv8/YOLO11 `.cvimodel` on the NPU — **detect, pose, segment or classify**, told apart by its outputs (§3b); decoding in Rust; the target class, threshold, NMS, minimum area, area of interest, frames to confirm and seconds to leave change live |
| Events of the target | `target_arrived` (`class`, `label`, `score`, `box`, `count`, and `keypoints` with a pose model) after `confirm_frames` frames in a row; `target_left` (`stayed_s`) after `leave_after_s` without it |
| Area of interest | one rectangle drawn on the page; the target counts when its base (bottom centre of the box) is inside it |
| Web page | port 80, login with the system account; sections Camera, YOLO, Network, MQTT, LLM, System, Terminal (§4) |
| MJPEG | `/stream.mjpg`, `/snapshot.jpg`; hardware JPEG, only encoding while someone watches |
| RTSP | `rtsp://user:password@camera/live`, port 554, TCP interleaved or UDP, Basic auth with the system account; hardware H.264 Constrained Baseline, CBR, only encoding while someone watches |
| WebRTC | WHEP (`POST /whep`, `DELETE /whep/<id>`), `str0m` with OpenSSL; the same H.264 as RTSP, no per-viewer encoding; the page falls back to MJPEG after 6 s |
| MQTT | Home Assistant discovery, state, events, telemetry, snapshots, a camera entity, commands (§5) |
| LLM/VLM | the frame of each arrival to an OpenAI-compatible chat endpoint or a WebSocket service (§6); off by default |
| Terminal | xterm.js in the page over a WebSocket + PTY running `login` (user and password asked, as over ssh); can be turned off |
| Robustness | `kill -9` safe: the next start reclaims what a dead process left (`docs/multimedia.md` §7); the watchdog resets a hung SoC; the crash log is kept by pstore |

## 3. Configuration reference (`/data/recamera/config.json`)

| Section | Keys (default) |
|---|---|
| `camera` | `sensor_mode` (`1280x960`; or `1920x1080`, `640x480`: restarts the pipeline), `brightness`, `contrast`, `saturation`, `hue` (50: the ISP's CSC), `sharpness` (50 = the tuning; 0 off, 100 doubles), `mirror`, `flip` |
| `detection` | `enabled` (true), `model` (file in `/data/models`), `target_class` (0), `threshold` (0.5), `nms` (0.45), `min_area` (0, fraction of the picture), `roi` (`{"x","y","w","h"}` relative, or `null`), `confirm_frames` (3), `leave_after_s` (2.0) |
| `stream` | `mjpeg_fps` (15), `jpeg_quality` (80) |
| `led` | `brightness` (0–255, the white LED) |
| `web` | `port` (80, 0 = off), `terminal` (true) |
| `rtsp` | `port` (554, 0 = off), `kbps` (2048), `gop` (30), `auth` (true) |
| `system` | `timezone` (`UTC`; a tz name) |
| `mqtt` | `enabled`, `host`, `port` (1883), `username`, `password`, `tls`, `base_topic` (`recamera`), `discovery` (true), `discovery_prefix` (`homeassistant`), `device_name` (`reCamera`), `camera_interval_s` (5, 0 = off) |
| `llm` | `enabled` (false), `endpoint`, `model`, `api_key`, `prompt` (`{label}` becomes the target's name), `max_tokens` (200), `timeout_s` (30), `min_interval_s` (60) |
| `telemetry` | `interval_s` (10) |
| `events` | `socket` (`/run/recamerad/events.sock`) |

Models live in `/data/models`: a `.cvimodel` for the cv181x NPU (RGB input) and a `.json`
next to it with `"classes": [...]`. `tools/recamera-convert` makes both from a YOLOv8/YOLO11
`.pt` or `.onnx` on a PC (Sophgo's TPU-MLIR in Docker; see its README). Ultralytics weights are AGPL-3.0, so none ship in the
image or the repository; `scripts/models.sh` extracts the four factory YOLO11n models from
the official Seeed image, and the YOLO page uploads your own.

## 3b. Model types

The daemon reads the task from the shapes of the model's outputs
(`recamera_detect::decode::layout`, tested on the host):

| Outputs | Task | Notes |
|---|---|---|
| per stride 8/16/32 a `[1,64,h,w]` box tensor + a `[1,C,h,w]` class tensor, INT8 | detect | the Sophgo export (the factory detection model): raw DFL, decoded here |
| one `[1, 4 + nc, N]` | detect | the Ultralytics export, already decoded (box centre/size in pixels, class scores) |
| one `[1, 4 + nc + K×3 (or ×2), N]` | pose | **K is deduced** (17 for people, 24 for the Ultralytics dog-pose dataset, any other) |
| `[1, 4 + nc + 32, N]` + `[1, 32, h, w]` prototypes | segment | masks computed only while the page shows them (CPU work) |
| `[1, nc]` | classify | its own input size (224×224 for YOLO11n-cls) from the centre square of the picture; "present" = the target class at or above the threshold |

Float or INT8 outputs both work. Optional `.json` fields, which win over the deduction:
`"task"` (`detect`, `pose`, `segment`, `classify`), `"kpt_shape"` (`[K, 2|3]`),
`"skeleton"` (pairs of keypoint indices from 0, for drawing; a 17-keypoint model without one
gets the COCO person skeleton) and `"kpt_names"`. A `.json` that lists another number of
classes than the model has is reported (log, events, YOLO page): the labels would be wrong.
The factory classify `.json` is such a case (1003 names for 1000 outputs).

Measured with RTSP and MJPEG open: detect 23.6 fps (ION peak 25.8 MB), pose 16.7 fps
(27.3 MB), segment 12 fps (36.0 MB), classify 30 fps.

**ION budget:** a model that leaves less than ~5.5 MB of ION free after the pipeline and the
encoders are open is unloaded at start (the stream goes on, the page says why): the H.264
encoder allocates its reconstruction frames at the first frame, and when that fails the
Sophgo encoder's error path hangs the codec.

## 4. The web page and its API

The page (`recamerad/web/`, plain HTML/JS/CSS, no framework) is built into the binary.

- **Login:** `POST /api/login` checks the system account against `/etc/shadow` (musl
  `crypt()`, `$5$`), 1 s delay on a wrong password, an `HttpOnly`, `SameSite=Strict`
  session cookie. `recamera`/`recamera` is only the default account. **Changing the
  password on the page changes the system one** (ssh included); the hash is also kept in
  `/data/recamera/passwords` and restored at boot by `S02passwords`, so it survives an A/B
  update.
- **Camera:** live video (WebRTC, MJPEG fallback) with the boxes drawn on a canvas (green =
  counts, grey = outside the area) from Server-Sent Events (`/live`), the area of interest,
  detection controls, picture controls (brightness, contrast, saturation, hue, sharpness,
  mirror, flip), white LED, stream rate and quality, the LLM switch, recent events.
- **YOLO:** the models in `/data/models` (title, task and classes), Use (the pipeline
  restarts with it in about a second; the page then reports what was loaded), Delete
  (never the active one), upload (`PUT /api/models/<name>`, streamed to disk, 32 MB limit,
  checked: `CviModel` header, valid `.json`, plain file names).
- **Network:** Wi-Fi state, addresses, saved networks (forget), scan and connect, over the
  `wpa_supplicant` control socket; changes are saved and copied to `/data/recamera`.
- **MQTT:** broker settings, Test broker (a connection attempt with the form values, not
  saved), connection state, the topics.
- **LLM:** endpoint, model, key, prompt, limits, Test with the current frame (shows the
  picture sent and the answer), the last answers.
- **System:** version, slot, addresses, uptime, temperature, load, memory, YOLO and camera
  rates; time zone; password; terminal on/off; **update** (the version in each slot, upload
  of a `recamera-update-*.rcu` into the slot not in use with its progress, reboot into it:
  `docs/emmc-layout.md` §6); reboot; the last 300 log lines; crash reports (pstore).
- **Terminal:** a login on the camera (`/bin/login` on a PTY): user and password, then that
  account's shell; up to 4 at once; the `Origin` is checked; stays open across sections.

| Endpoint | Method |
|---|---|
| `/api/login`, `/api/logout`, `/api/session` | POST, POST, GET |
| `/api/config` | GET, POST (validated, saved, applied) |
| `/api/status`, `/api/labels`, `/api/model`, `/api/logs`, `/api/crashes[/<dir>/<file>]`, `/api/timezones` | GET |
| `/api/models`, `/api/models/<name>` | GET; PUT, DELETE |
| `/api/password`, `/api/reboot` | POST |
| `/api/network`, `/api/network/scan`, `/api/network/connect`, `/api/network/forget` | GET, POST… |
| `/api/mqtt`, `/api/mqtt/test` | GET, POST |
| `/api/llm`, `/api/llm/test` | GET, POST |
| `/api/terminal` | GET (WebSocket) |
| `/api/update`, `/api/update/reboot` | GET, POST (the `.rcu` as the body); POST |
| `/live` (SSE), `/stream.mjpg`, `/snapshot.jpg`, `/whep` | GET, GET, GET, POST/DELETE |

## 5. MQTT and Home Assistant

`rumqttc` with the image's OpenSSL for TLS (`native-tls`, system CA bundle). The device id
is `recamera_` + the last three bytes of the MAC. Topics under `<base_topic>/<device id>/`:

| Topic | Contents |
|---|---|
| `status` | `online` / `offline` (last will), retained |
| `target` | `ON` / `OFF`, retained |
| `event` | the arrival/departure JSON |
| `telemetry` | temperature, load, memory, YOLO and stream rates, retained |
| `snapshot` | a JPEG at every arrival |
| `camera` | a JPEG every `camera_interval_s` |
| `llm` | the last LLM answer (JSON), retained |
| `detection`, `led`, `led/brightness` | states, retained |
| `detection/set`, `led/set`, `led/brightness/set` | commands |

With discovery on, Home Assistant shows one device with: Target (occupancy), Camera, Last
arrival (image), LLM answer, SoC temperature, Load, Free memory, YOLO rate, Detection
(switch), White LED (light), and "Visit device" opening the page. The MQTT camera entity
is a still picture by design (Home Assistant's MQTT camera has no stream); for live video
add the RTSP URL with the **Generic Camera** integration (Home Assistant restreams it over
WebRTC with its built-in go2rtc).

Pitfall: `rumqttc`'s `set_max_packet_size(incoming, outgoing)` — the outgoing limit must
fit a JPEG (~150 KB); the default is 10 KiB.

## 6. The LLM/VLM call

Off by default (it costs money per call). When on, each arrival (at most one request per
`min_interval_s`, one at a time) sends the frame of that moment as
`data:image/jpeg;base64,…` with the detection. Two kinds of endpoint, by URL scheme:

- **`http(s)://…/chat/completions`:** the OpenAI chat API, which OpenAI, Ollama (local and
  cloud), OpenRouter, vLLM and most providers speak. One user message with the prompt, the
  detection and the picture; the answer is `choices[0].message.content`. If the server
  refuses `max_tokens` and asks for `max_completion_tokens` (newer OpenAI models), the
  request is retried with it. HTTP/1.1 through `hyper`'s client over the same native TLS.
- **`ws(s)://…`:** a service of your own: one JSON text message out (`type`, `model`,
  `prompt`, `max_tokens`, `detection`, `image`); the first message back is the answer (its
  `text`, `answer` or `content` field when it is JSON).

The key goes as `Authorization: Bearer <key>`. Answers become `llm` events (page, event
socket, MQTT). Tested against Ollama's cloud OpenAI-compatible endpoint with a vision
model: 3.6–5.9 s per answer from the camera.

## 7. Network and Wi-Fi

`wpa_supplicant` reads `/etc/wpa_supplicant.conf`, restored at boot from
`/data/recamera/wpa_supplicant.conf` (which survives A/B updates); the page saves through
the control socket (`SAVE_CONFIG`) and copies the result back to `/data`. Reading the
configuration directly from `/data` made the boot handshake take minutes (cause unknown;
`docs/hardware.md` §2). The USB-C network (192.168.42.1) is always there as a way in.

## 8. Measurements along the way

| Stage | Measured on the camera |
|---|---|
| Skeleton | RSS 1.4 MB, starts at boot, clean reload and stop |
| Pipeline | 30.0 fps on both VPSS channels, 6 % CPU, RSS 3.0 MB, ION 14 MB; `kill -9` + restart loses no memory |
| YOLO | 26.1 fps at 38.3 ms (the C reference: 25.0 fps), 7 % CPU, RSS 6.4 MB, ION 20.7 MB; identical scores to the C code on the same frame, boxes identical except on exact score ties (C `qsort` is unstable) |
| Events | arrival in 0.1 s, departure 2 s after leaving the area, no pipeline restart when the ROI changes |
| MJPEG | 15 fps (~110 KB per frame) with YOLO at 25 fps; 8 % CPU without viewers, 27 % with the video open |
| RTSP | 301 frames in 10 s over TCP and UDP, 1280×720 Constrained Baseline, decoded without errors; everything at once (RTSP + MJPEG + YOLO): 29 % CPU, RSS 7.3 MB, ION 26.4 MB |
| WebRTC | 1280×720 at 30 fps, 0 packets lost, ~2 Mbit/s (browser `getStats()`), YOLO at 24.7 fps |
| Picture | mean luminance 52 / 103 / 156 at brightness 30 / 50 / 70 |
| Wi-Fi | `COMPLETED` within 20 s of power-on |

## 9. Not done yet

- Wi-Fi onboarding without a cable (a temporary access point).
- Bluetooth (GATT) configuration.
- Policies in the daemon: automatic LED from the ISP's ISO, the thermal degradation order
  (`docs/hardware.md` §7b), the daemon feeding the watchdog itself.
- Man pages (the image has no formatter; it needs `mandoc` in the toolchain image).
