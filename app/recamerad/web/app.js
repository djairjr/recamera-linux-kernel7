// reCamera page: login, the sections of the side bar, the camera view with
// the boxes and the area of interest (ROI), the detection controls and the system
// status. No framework; talks to recamerad (/api/*, /stream.mjpg, /live).
"use strict";

const $ = (s) => document.querySelector(s);
const $$ = (s) => [...document.querySelectorAll(s)];
let config = null;
let dets = { boxes: [] };
let modelInfo = {};  // the running model: task, classes, keypoints, skeleton (/api/model)
let live = null;

async function api(path, body) {
  const r = await fetch(path, body === undefined ? {} : {
    method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(body),
  });
  if (r.status === 401 && path !== "/api/login") { showLogin(); throw new Error("login required"); }
  const j = await r.json().catch(() => ({}));
  if (!r.ok) throw new Error(j.error || r.statusText);
  return j;
}

// ---- login -----------------------------------------------------------------

function showLogin() {
  closeTerminal();
  stopLive();
  stopVideo();
  $("#app").hidden = true;
  $("#login").hidden = false;
}

$("#login-form").addEventListener("submit", async (e) => {
  e.preventDefault();
  const f = new FormData(e.target);
  $("#login-error").hidden = true;
  try {
    await api("/api/login", { user: f.get("user"), password: f.get("password") });
    start();
  } catch (err) {
    $("#login-error").textContent = err.message;
    $("#login-error").hidden = false;
  }
});

$("#logout").addEventListener("click", async () => {
  await api("/api/logout", {}).catch(() => {});
  showLogin();
});

// ---- sections ----------------------------------------------------------------

function route() {
  const page = (location.hash || "#camera").slice(1);
  $$(".page").forEach((p) => p.classList.toggle("current", p.id === "page-" + page));
  $$("#side a").forEach((a) => a.classList.toggle("current", a.dataset.page === page));
  // the video (and the encoder on the camera) only while the camera page is shown
  if (page === "camera") startVideo(); else stopVideo();
  if (page === "system") { refreshStatus(); loadSystem(); }
  if (page === "yolo") loadModels();
  if (page === "network") loadNetwork();
  if (page === "mqtt") loadMqtt();
  if (page === "llm") loadLlm();
  // the shell stays open on the other pages; logging out closes it
  if (page === "terminal") openTerminal();
}
window.addEventListener("hashchange", route);

// ---- video: WebRTC (WHEP), MJPEG if it cannot connect -----------------------------

let pc = null, whepUrl = null, videoOn = false;

function showMode(m) { $("#mode").textContent = m; }

function mjpeg() {
  stopWebrtc();
  $("#video").hidden = true;
  const img = $("#stream");
  img.hidden = false;
  if (!img.src.endsWith("/stream.mjpg")) img.src = "/stream.mjpg";
  showMode("MJPEG");
}

async function webrtc() {
  const v = $("#video");
  pc = new RTCPeerConnection();
  pc.addTransceiver("video", { direction: "recvonly" });
  pc.ontrack = (e) => { v.srcObject = e.streams[0] || new MediaStream([e.track]); };
  pc.onconnectionstatechange = () => {
    if (pc && (pc.connectionState === "failed" || pc.connectionState === "disconnected")) mjpeg();
  };
  await pc.setLocalDescription(await pc.createOffer());
  // no trickle: send the offer with the candidates gathered so far
  await new Promise((ok) => {
    if (pc.iceGatheringState === "complete") return ok();
    pc.onicegatheringstatechange = () => pc.iceGatheringState === "complete" && ok();
    setTimeout(ok, 1500);
  });
  const r = await fetch("/whep", { method: "POST", headers: { "Content-Type": "application/sdp" }, body: pc.localDescription.sdp });
  if (r.status !== 201) throw new Error("whep " + r.status);
  whepUrl = r.headers.get("Location");
  await pc.setRemoteDescription({ type: "answer", sdp: await r.text() });
  // playing within a few seconds, or MJPEG
  await new Promise((ok, fail) => {
    const t = setTimeout(() => fail(new Error("no video")), 6000);
    v.onplaying = () => { clearTimeout(t); ok(); };
  });
  $("#stream").hidden = true;
  $("#stream").removeAttribute("src");
  v.hidden = false;
  showMode("WebRTC");
}

function stopWebrtc() {
  if (whepUrl) fetch(whepUrl, { method: "DELETE" }).catch(() => {});
  whepUrl = null;
  if (pc) { pc.close(); pc = null; }
  $("#video").srcObject = null;
}

async function startVideo() {
  if (videoOn) return;
  videoOn = true;
  showMode("…");
  if (!window.RTCPeerConnection) return mjpeg();
  try { await webrtc(); } catch (e) { console.log("webrtc:", e.message); if (videoOn) mjpeg(); }
}

function stopVideo() {
  videoOn = false;
  stopWebrtc();
  $("#stream").removeAttribute("src");
}

// ---- configuration controls ---------------------------------------------------

const get = (o, k) => k.split(".").reduce((a, p) => (a == null ? a : a[p]), o);
const set = (o, k, v) => { const p = k.split("."); const last = p.pop(); p.reduce((a, q) => a[q], o)[last] = v; };

function showControls() {
  for (const el of $$("#controls [data-k]")) {
    const v = get(config, el.dataset.k);
    if (el.type === "checkbox") el.checked = !!v; else el.value = v;
    const out = el.parentElement.querySelector("output");
    if (out) out.textContent = el.value;
  }
}

let saveTimer = null;
async function save() {
  $("#save-state").textContent = "Saving…";
  try {
    await api("/api/config", config);
    $("#save-state").textContent = "Saved";
  } catch (err) {
    $("#save-state").textContent = "Not saved: " + err.message;
  }
}
function saveSoon() { clearTimeout(saveTimer); saveTimer = setTimeout(save, 400); }

$("#controls").addEventListener("input", (e) => {
  const el = e.target;
  if (!el.dataset.k) return;
  const v = el.type === "checkbox" ? el.checked : ("num" in el.dataset ? Number(el.value) : el.value);
  set(config, el.dataset.k, v);
  const out = el.parentElement.querySelector("output");
  if (out) out.textContent = el.value;
  saveSoon();
});

// ---- picture -----------------------------------------------------------------------

$("#picture-reset").addEventListener("click", () => {
  for (const k of ["brightness", "contrast", "saturation", "hue", "sharpness"]) config.camera[k] = 50;
  config.camera.mirror = false;
  config.camera.flip = false;
  showControls();
  save();
});

// ---- models ------------------------------------------------------------------------

function fmtSize(b) { return b > 1 << 20 ? (b / (1 << 20)).toFixed(1) + " MB" : Math.round(b / 1024) + " KB"; }

// the detector decodes YOLOv8/11 detection outputs: classify, pose and
// segment models (same family, other heads) cannot be used
async function loadModels() {
  // the result of a model switch, kept across the page reload that follows it
  try {
    const msg = sessionStorage.getItem("model-state");
    if (msg) { $("#model-state").textContent = msg; sessionStorage.removeItem("model-state"); }
  } catch {}
  const list = await api("/api/models").catch(() => []);
  const ul = $("#models");
  ul.innerHTML = list.length ? "" : "<li class=muted>No model in /data/models</li>";
  for (const m of list) {
    const li = document.createElement("li");
    li.classList.toggle("active", m.active);
    const name = document.createElement("span");
    name.textContent = (m.title || m.name) + ` · ${m.task}` + (m.classes ? ` · ${m.classes} classes` : "") +
      ` · ${fmtSize(m.size)}`;
    name.title = m.name;
    li.append(name);
    if (m.active) {
      li.append("active");
    } else {
      const use = document.createElement("button");
      use.type = "button";
      use.textContent = "Use";
      use.onclick = async () => {
        const st = $("#model-state");
        config.detection.model = m.name;
        config.detection.target_class = 0;
        st.textContent = `Loading ${m.name}…`;
        try {
          await api("/api/config", config);
        } catch (err) { st.textContent = `${m.name}: ${err.message}`; return; }
        // the pipeline restarts with the model: wait for it, then reload the
        // whole page so nothing on it describes the previous model
        for (let i = 0; i < 15; i++) {
          await new Promise((r) => setTimeout(r, 700));
          await loadModelInfo();
          if (modelInfo.model === m.name || modelInfo.error) break;
        }
        const msg = modelInfo.model === m.name
          ? `${m.name}: ${modelInfo.task}, ${modelInfo.classes.length} classes` +
            (modelInfo.keypoints ? `, ${modelInfo.keypoints} keypoints` : "") +
            `, input ${modelInfo.input[0]}×${modelInfo.input[1]}` +
            (modelInfo.warning ? `. Warning: ${modelInfo.warning}` : "")
          : `${m.name}: not loaded` + (modelInfo.error ? `: ${modelInfo.error}` : " (see the System log)");
        try { sessionStorage.setItem("model-state", msg); } catch {}
        location.reload();
      };
      const del = document.createElement("button");
      del.type = "button";
      del.textContent = "Delete";
      del.onclick = async () => {
        if (!confirm(`Delete ${m.name}?`)) return;
        await fetch("/api/models/" + encodeURIComponent(m.name), { method: "DELETE" });
        loadModels();
      };
      li.append(use, del);
    }
    ul.append(li);
  }
}

$("#model-file").addEventListener("change", async (e) => {
  const state = $("#model-state");
  for (const f of e.target.files) {
    state.textContent = `Sending ${f.name}…`;
    const r = await fetch("/api/models/" + encodeURIComponent(f.name), { method: "PUT", body: f });
    const j = await r.json().catch(() => ({}));
    state.textContent = r.ok ? `${f.name} saved` : `${f.name}: ${j.error || r.statusText}`;
    if (!r.ok) break;
  }
  e.target.value = "";
  loadModels();
});

async function loadModelInfo() {
  modelInfo = await api("/api/model").catch(() => ({}));
}

async function loadLabels() {
  const labels = await api("/api/labels").catch(() => []);
  const sel = $('[data-k="detection.target_class"]');
  sel.innerHTML = "";
  const n = Math.max(labels.length, config.detection.target_class + 1);
  for (let i = 0; i < n; i++) sel.add(new Option(labels[i] || "class " + i, i));
  sel.value = config.detection.target_class;
}

// ---- the picture: boxes and area of interest ------------------------------------

const canvas = $("#overlay");
const ctx = canvas.getContext("2d");
let drawing = false, drag = null;

function draw() {
  const w = canvas.clientWidth, h = canvas.clientHeight, dpr = window.devicePixelRatio || 1;
  if (canvas.width !== Math.round(w * dpr)) { canvas.width = Math.round(w * dpr); canvas.height = Math.round(h * dpr); }
  ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
  ctx.clearRect(0, 0, w, h);
  const roi = drag || (config && config.detection.roi);
  if (roi) {
    ctx.fillStyle = "rgba(250, 204, 21, 0.12)";
    ctx.strokeStyle = "#facc15";
    ctx.lineWidth = 2;
    ctx.setLineDash([8, 6]);
    ctx.fillRect(roi.x * w, roi.y * h, roi.w * w, roi.h * h);
    ctx.strokeRect(roi.x * w, roi.y * h, roi.w * w, roi.h * h);
    ctx.setLineDash([]);
  }
  ctx.font = "13px system-ui, sans-serif";
  // classify: the best classes over the picture
  if (dets.task === "classify" && dets.top) {
    let y = 8;
    for (const t of dets.top) {
      const s = `${t.label} ${Math.round(t.score * 100)}%`;
      const tw = ctx.measureText(s).width + 10;
      ctx.fillStyle = t.label === dets.target && dets.present ? "#22c55e" : "rgba(0, 0, 0, 0.6)";
      ctx.fillRect(8, y, tw, 20);
      ctx.fillStyle = "#fff";
      ctx.fillText(s, 13, y + 15);
      y += 24;
    }
  }
  for (const b of dets.boxes || []) {
    if (b.mask) drawMask(b.mask, w, h, b.counts);
    if (b.kpts) drawPose(b.kpts, w, h);
    if (dets.task === "classify") continue;
    const x = b.x1 * w, y = b.y1 * h, bw = (b.x2 - b.x1) * w, bh = (b.y2 - b.y1) * h;
    ctx.strokeStyle = b.counts ? "#22c55e" : "rgba(200, 200, 200, 0.8)";
    ctx.lineWidth = b.counts ? 3 : 1.5;
    ctx.strokeRect(x, y, bw, bh);
    const t = `${dets.target || b.class} ${Math.round(b.score * 100)}%`;
    const tw = ctx.measureText(t).width + 8;
    ctx.fillStyle = b.counts ? "#22c55e" : "rgba(80, 80, 80, 0.8)";
    ctx.fillRect(x, Math.max(0, y - 18), tw, 18);
    ctx.fillStyle = "#fff";
    ctx.fillText(t, x + 4, Math.max(13, y - 5));
    if (b.counts) { // the base of the box: what the area test uses
      ctx.beginPath();
      ctx.arc((b.x1 + b.x2) / 2 * w, b.y2 * h, 4, 0, 2 * Math.PI);
      ctx.fill();
    }
  }
}
new ResizeObserver(draw).observe(canvas);

// segment: the mask bits (rows on whole bytes, most significant bit first)
// over the rectangle they cover
const maskCanvas = document.createElement("canvas");
function drawMask(m, w, h, counts) {
  const bits = atob(m.bits), stride = Math.ceil(m.w / 8);
  maskCanvas.width = m.w; maskCanvas.height = m.h;
  const mc = maskCanvas.getContext("2d"), img = mc.createImageData(m.w, m.h);
  const [r, g, bl] = counts ? [34, 197, 94] : [200, 200, 200];
  for (let y = 0; y < m.h; y++) {
    for (let x = 0; x < m.w; x++) {
      if (bits.charCodeAt(y * stride + (x >> 3)) & (0x80 >> (x & 7))) {
        const i = (y * m.w + x) * 4;
        img.data[i] = r; img.data[i + 1] = g; img.data[i + 2] = bl; img.data[i + 3] = 110;
      }
    }
  }
  mc.putImageData(img, 0, 0);
  ctx.imageSmoothingEnabled = true;
  ctx.drawImage(maskCanvas, m.x1 * w, m.y1 * h, (m.x2 - m.x1) * w, (m.y2 - m.y1) * h);
}

// pose: the skeleton of the model (its .json, or COCO for 17 keypoints),
// then the visible keypoints
function drawPose(k, w, h) {
  const seen = (p) => p && p[2] >= 0.5;
  ctx.strokeStyle = "#38bdf8";
  ctx.lineWidth = 2;
  for (const [a, b] of modelInfo.skeleton || []) {
    if (!seen(k[a]) || !seen(k[b])) continue;
    ctx.beginPath();
    ctx.moveTo(k[a][0] * w, k[a][1] * h);
    ctx.lineTo(k[b][0] * w, k[b][1] * h);
    ctx.stroke();
  }
  ctx.fillStyle = "#f97316";
  k.forEach((p, i) => {
    if (!seen(p)) return;
    ctx.beginPath();
    ctx.arc(p[0] * w, p[1] * h, 3.5, 0, 2 * Math.PI);
    ctx.fill();
    const n = modelInfo.kpt_names && modelInfo.kpt_names[i];
    if (n && w > 700) { ctx.fillStyle = "#fff"; ctx.fillText(n, p[0] * w + 5, p[1] * h - 4); ctx.fillStyle = "#f97316"; }
  });
}

function setDrawing(on) {
  drawing = on;
  canvas.classList.toggle("drawing", on);
  $("#roi-draw").classList.toggle("active", on);
  $("#roi-hint").hidden = !on;
}
$("#roi-draw").addEventListener("click", () => setDrawing(!drawing));
$("#roi-clear").addEventListener("click", () => { config.detection.roi = null; setDrawing(false); draw(); save(); });

const rel = (e) => {
  const r = canvas.getBoundingClientRect();
  return [Math.min(1, Math.max(0, (e.clientX - r.left) / r.width)), Math.min(1, Math.max(0, (e.clientY - r.top) / r.height))];
};
canvas.addEventListener("pointerdown", (e) => {
  if (!drawing) return;
  canvas.setPointerCapture(e.pointerId);
  const [x, y] = rel(e);
  drag = { x, y, w: 0, h: 0, x0: x, y0: y };
});
canvas.addEventListener("pointermove", (e) => {
  if (!drag) return;
  const [x, y] = rel(e);
  drag.x = Math.min(x, drag.x0); drag.y = Math.min(y, drag.y0);
  drag.w = Math.abs(x - drag.x0); drag.h = Math.abs(y - drag.y0);
  draw();
});
canvas.addEventListener("pointerup", () => {
  if (!drag) return;
  const r = drag; drag = null;
  if (r.w > 0.02 && r.h > 0.02) {
    const q = (v) => Math.round(v * 1000) / 1000;
    config.detection.roi = { x: q(r.x), y: q(r.y), w: q(Math.min(r.w, 1 - r.x)), h: q(Math.min(r.h, 1 - r.y)) };
    save();
  }
  setDrawing(false);
  draw();
});

// ---- live data (Server-Sent Events) ----------------------------------------------

function addEvent(ev) {
  const li = document.createElement("li");
  const t = new Date(ev.ts).toLocaleTimeString();
  li.innerHTML = `<time>${t}</time>`;
  li.append(ev.type === "target_arrived"
    ? `${ev.label} arrived (${Math.round(ev.score * 100)}%, ${ev.count} in the area)`
    : ev.type === "llm" ? (ev.ok ? `LLM: ${ev.text}` : `LLM failed: ${ev.error}`)
    : `${ev.label} left after ${ev.stayed_s ?? "?"} s${ev.reason ? " (" + ev.reason + ")" : ""}`);
  if (ev.type === "llm" && !ev.ok) li.classList.add("bad");
  const ul = $("#events");
  ul.prepend(li);
  while (ul.children.length > 12) ul.lastChild.remove();
}

function startLive() {
  stopLive();
  live = new EventSource("/live");
  live.addEventListener("dets", (e) => {
    dets = JSON.parse(e.data);
    const p = $("#presence");
    p.textContent = dets.present ? `${dets.target} present` : `no ${dets.target || "target"}`;
    p.classList.toggle("on", !!dets.present);
    draw();
  });
  live.onmessage = (e) => {
    const ev = JSON.parse(e.data);
    if (ev.type === "target_arrived" || ev.type === "target_left" || ev.type === "llm") addEvent(ev);
    if (ev.type === "llm" && $("#page-llm").classList.contains("current")) loadLlmLast();
    if (ev.type === "media") $("#rates").textContent = $("#mode").textContent === "WebRTC"
      ? `YOLO ${ev.yolo_fps} fps · video ${ev.h264_fps} fps, ${ev.h264_kbps} kbit/s`
      : `YOLO ${ev.yolo_fps} fps · video ${ev.mjpeg_fps} fps, ${ev.jpeg_kb} KB/frame`;
    if (ev.type === "config_reloaded" && ev.from !== "web") reloadConfig();
  };
}
function stopLive() { if (live) { live.close(); live = null; } }

// ---- system --------------------------------------------------------------------------

function fmtUptime(s) {
  const d = Math.floor(s / 86400), h = Math.floor(s % 86400 / 3600), m = Math.floor(s % 3600 / 60);
  return (d ? d + " d " : "") + h + " h " + m + " min";
}

// ---- network -----------------------------------------------------------------------------

function kvTable(t, rows) {
  t.innerHTML = "";
  for (const [k, v] of rows) {
    const tr = t.insertRow();
    tr.insertCell().outerHTML = `<th>${k}</th>`;
    tr.insertCell().textContent = v;
  }
}

function bars(dbm) {
  const n = dbm >= -55 ? 4 : dbm >= -65 ? 3 : dbm >= -75 ? 2 : 1;
  return "▂▄▆█".slice(0, n).padEnd(4, "·");
}

async function loadNetwork() {
  const n = await api("/api/network").catch(() => null);
  if (!n) return;
  const st = (n.wifi && n.wifi.status) || {};
  const sig = (n.wifi && n.wifi.signal) || {};
  kvTable($("#wifi-status"), n.wifi.error ? [["State", n.wifi.error]] : [
    ["State", st.wpa_state || "–"],
    ["Network", st.ssid || "–"],
    ["Address", st.ip_address || "–"],
    ["Signal", sig.RSSI ? `${bars(+sig.RSSI)} ${sig.RSSI} dBm` : "–"],
    ["Link", sig.LINKSPEED ? `${sig.LINKSPEED} Mbit/s, ${st.freq} MHz` : "–"],
    ["Security", st.key_mgmt || "–"],
  ]);
  kvTable($("#net-addresses"), n.addresses.map((a) => [a.if, a.ip]));
  const ul = $("#wifi-saved");
  ul.innerHTML = n.wifi.saved.length ? "" : "<li class=muted>None</li>";
  for (const w of n.wifi.saved) {
    const li = document.createElement("li");
    const s = document.createElement("span");
    s.textContent = w.ssid + (w.current ? " (connected)" : "");
    li.append(s);
    const b = document.createElement("button");
    b.type = "button";
    b.textContent = "Forget";
    b.onclick = async () => {
      if (!confirm(`Forget ${w.ssid}?` + (w.current ? " The camera leaves this network now." : ""))) return;
      await api("/api/network/forget", { id: w.id }).catch((e) => alert(e.message));
      loadNetwork();
    };
    li.append(b);
    ul.append(li);
  }
}

$("#wifi-scan").addEventListener("click", async () => {
  const st = $("#wifi-scan-state");
  st.textContent = "Scanning…";
  const list = await api("/api/network/scan", {}).catch((e) => { st.textContent = e.message; return null; });
  if (!list) return;
  st.textContent = `${list.length} networks`;
  const ul = $("#wifi-list");
  ul.innerHTML = "";
  for (const w of list) {
    const li = document.createElement("li");
    const s = document.createElement("span");
    s.textContent = `${bars(w.signal)} ${w.ssid}${w.secure ? " 🔒" : ""} · ${w.freq < 3000 ? "2.4" : "5"} GHz`;
    li.append(s);
    const b = document.createElement("button");
    b.type = "button";
    b.textContent = "Connect";
    b.onclick = () => {
      $("#wifi-ssid").textContent = w.ssid;
      $("#wifi-form").hidden = false;
      $("#wifi-form").psk.value = "";
      $("#wifi-form").psk.focus();
    };
    li.append(b);
    ul.append(li);
  }
});

$("#wifi-form").addEventListener("submit", async (e) => {
  e.preventDefault();
  const ssid = $("#wifi-ssid").textContent;
  $("#wifi-state").textContent = `Connecting to ${ssid}…`;
  try {
    await api("/api/network/connect", { ssid, psk: e.target.psk.value });
    e.target.hidden = true;
    $("#wifi-state").textContent = `Saved; connecting to ${ssid}.`;
    setTimeout(loadNetwork, 8000);
  } catch (err) { $("#wifi-state").textContent = err.message; }
});

// ---- MQTT ------------------------------------------------------------------------------------

async function loadMqtt() {
  const f = $("#mqtt-form");
  for (const el of f.querySelectorAll("[data-m]")) {
    const v = config.mqtt[el.dataset.m];
    if (el.type === "checkbox") el.checked = !!v; else el.value = v;
  }
  const s = await api("/api/mqtt").catch(() => null);
  if (!s) return;
  $("#mqtt-base").textContent = `${config.mqtt.base_topic}/${s.device_id}`;
  const st = s.state || {};
  kvTable($("#mqtt-status"), [
    ["State", !s.enabled ? "off" : st.connected ? "connected" : st.error ? "not connected" : "connecting…"],
    ["Error", st.error || "–"],
    ["Device id", s.device_id],
  ]);
}

function mqttForm() {
  const m = { ...config.mqtt };
  for (const el of $("#mqtt-form").querySelectorAll("[data-m]")) {
    m[el.dataset.m] = el.type === "checkbox" ? el.checked : el.type === "number" ? Number(el.value) : el.value.trim();
  }
  return m;
}

$("#mqtt-test").addEventListener("click", async () => {
  const st = $("#mqtt-test-state");
  st.textContent = "Testing…";
  const r = await api("/api/mqtt/test", mqttForm()).catch((e) => ({ ok: false, error: e.message }));
  st.textContent = r.ok ? "The broker accepted the connection." : "Failed: " + r.error;
});

$("#mqtt-form").addEventListener("submit", async (e) => {
  e.preventDefault();
  config.mqtt = mqttForm();
  $("#mqtt-save").textContent = "Saving…";
  try {
    await api("/api/config", config);
    $("#mqtt-save").textContent = "Saved";
    setTimeout(loadMqtt, 3000);
  } catch (err) { $("#mqtt-save").textContent = "Not saved: " + err.message; }
});

// ---- LLM -------------------------------------------------------------------------------------

function llmForm() {
  const l = { ...config.llm };
  for (const el of $("#llm-form").querySelectorAll("[data-l]")) {
    l[el.dataset.l] = el.type === "checkbox" ? el.checked : el.type === "number" ? Number(el.value) : el.value.trim();
  }
  return l;
}

async function loadLlm() {
  for (const el of $("#llm-form").querySelectorAll("[data-l]")) {
    const v = config.llm[el.dataset.l];
    if (el.type === "checkbox") el.checked = !!v; else el.value = v;
  }
  loadLlmLast();
}

async function loadLlmLast() {
  const list = await api("/api/llm").catch(() => []);
  const ul = $("#llm-last");
  ul.innerHTML = list.length ? "" : "<li class=muted>None yet</li>";
  for (const ev of list) {
    const li = document.createElement("li");
    li.innerHTML = `<time>${new Date(ev.ts).toLocaleTimeString()}</time>`;
    li.append(ev.ok ? `${ev.text} (${(ev.ms / 1000).toFixed(1)} s, ${ev.trigger})` : `failed: ${ev.error} (${ev.trigger})`);
    if (!ev.ok) li.classList.add("bad");
    ul.append(li);
  }
}

$("#llm-test").addEventListener("click", async () => {
  const st = $("#llm-test-state"), ans = $("#llm-test-answer"), img = $("#llm-test-img");
  st.textContent = "Asking…";
  ans.textContent = "";
  $("#llm-test").disabled = true;
  const r = await api("/api/llm/test", llmForm()).catch((e) => ({ ok: false, error: e.message }));
  $("#llm-test").disabled = false;
  if (r.image) { img.src = r.image; img.hidden = false; }
  st.textContent = r.ok ? `Answer in ${(r.ms / 1000).toFixed(1)} s:` : "Failed:";
  ans.textContent = r.ok ? r.text : r.error;
  ans.classList.toggle("error", !r.ok);
});

$("#llm-form").addEventListener("submit", async (e) => {
  e.preventDefault();
  const old = config.llm;
  config.llm = llmForm();
  $("#llm-save").textContent = "Saving…";
  try {
    await api("/api/config", config);
    $("#llm-save").textContent = "Saved";
    showControls();
  } catch (err) { config.llm = old; $("#llm-save").textContent = "Not saved: " + err.message; }
});

// ---- terminal (xterm.js, WebSocket /api/terminal) ---------------------------------------------

let term = null, fit = null, termWs = null;

function openTerminal() {
  if (termWs) { fit.fit(); return; }
  if (!term) {
    term = new Terminal({ cursorBlink: true, fontSize: 14, scrollback: 5000 });
    fit = new FitAddon.FitAddon();
    term.loadAddon(fit);
    term.open($("#term"));
    term.onData((d) => { if (termWs && termWs.readyState === 1) termWs.send(new TextEncoder().encode(d)); });
    term.onResize(({ cols, rows }) => {
      if (termWs && termWs.readyState === 1) termWs.send(JSON.stringify({ resize: [cols, rows] }));
    });
    new ResizeObserver(() => { if ($("#page-terminal").classList.contains("current")) fit.fit(); }).observe($("#term"));
  }
  fit.fit();
  const ws = new WebSocket(`${location.protocol === "https:" ? "wss" : "ws"}://${location.host}/api/terminal`);
  ws.binaryType = "arraybuffer";
  termWs = ws;
  $("#term-state").textContent = "connecting…";
  ws.onopen = () => {
    $("#term-state").textContent = "connected";
    ws.send(JSON.stringify({ resize: [term.cols, term.rows] }));
    term.focus();
  };
  ws.onmessage = (e) => { if (typeof e.data !== "string") term.write(new Uint8Array(e.data)); };
  ws.onclose = () => {
    if (termWs === ws) termWs = null;
    $("#term-state").textContent = config.web.terminal ? "closed" : "off (System page)";
  };
}

function closeTerminal() {
  if (termWs) { const ws = termWs; termWs = null; ws.close(); }
}

$("#term-reconnect").addEventListener("click", () => { closeTerminal(); term && term.reset(); openTerminal(); });

// ---- system actions --------------------------------------------------------------------

async function loadSystem() {
  const zones = await api("/api/timezones").catch(() => []);
  const sel = $("#timezone");
  sel.innerHTML = "";
  for (const z of ["UTC", ...zones.filter((z) => z !== "UTC")]) sel.add(new Option(z, z));
  sel.value = config.system.timezone;
  $("#terminal-on").checked = config.web.terminal;
  loadUpdate();
  loadLog();
  loadCrashes();
}

$("#timezone").addEventListener("change", (e) => { config.system.timezone = e.target.value; save(); });
$("#terminal-on").addEventListener("change", (e) => {
  config.web.terminal = e.target.checked;
  if (!config.web.terminal) closeTerminal();
  save();
});

$("#password-form").addEventListener("submit", async (e) => {
  e.preventDefault();
  const f = new FormData(e.target);
  const st = $("#password-state");
  if (f.get("new") !== f.get("again")) { st.textContent = "The new passwords differ"; return; }
  try {
    await api("/api/password", { current: f.get("current"), new: f.get("new") });
    st.textContent = "Password changed";
    e.target.reset();
  } catch (err) { st.textContent = err.message; }
});

$("#reboot").addEventListener("click", async () => {
  if (!confirm("Reboot the camera? It comes back in about a minute.")) return;
  await api("/api/reboot", {}).catch(() => {});
  $("#reboot").textContent = "Rebooting…";
  $("#reboot").disabled = true;
});

// ---- system update (A8d) ----------------------------------------------------------------

async function loadUpdate() {
  const u = await api("/api/update").catch(() => null);
  if (!u) return;
  const slot = (s) => (s ? s.toUpperCase() : "–");
  kvTable($("#update-slots"), u.error ? [["Update", u.error]] : [
    [`Running (slot ${slot(u.running)})`, u.running_version || "?"],
    [`Other (slot ${slot(u.inactive)})`, u.inactive_version || "–"],
  ]);
  showUpdate(u.state);
  return u;
}

function showUpdate(st) {
  const p = $("#update-progress"), t = $("#update-state");
  const mb = (b) => (b / 1048576).toFixed(1) + " MB";
  const active = ["receiving", "verifying", "finishing"].includes(st.phase);
  p.hidden = !active;
  if (st.phase === "receiving") {
    p.max = st.total || 1; p.value = st.written;
    t.textContent = `Writing slot ${(st.target || "").toUpperCase()}: ${mb(st.written)} of ${mb(st.total)} ` +
      `(${mb(st.received)} received)`;
  } else if (st.phase === "verifying") {
    p.removeAttribute("value"); t.textContent = "Reading the slot back to check it…";
  } else if (st.phase === "finishing") {
    p.removeAttribute("value"); t.textContent = "Resizing the file system and writing the kernel…";
  } else if (st.phase === "ready" || st.phase === "error") {
    t.textContent = st.message;
  }
  t.classList.toggle("error", st.phase === "error");
  $("#update-reboot").hidden = st.phase !== "ready";
}

$("#update-file").addEventListener("change", (e) => {
  const f = e.target.files[0];
  if (!f) return;
  if (!confirm(`Write ${f.name} into the slot not in use? The camera keeps running meanwhile.`)) {
    e.target.value = ""; return;
  }
  const t = $("#update-state"), p = $("#update-progress");
  // the upload itself (browser side), then the daemon's state while it writes
  const xhr = new XMLHttpRequest();
  xhr.open("POST", "/api/update");
  xhr.upload.onprogress = (ev) => {
    if (ev.lengthComputable && ev.loaded < ev.total) {
      p.hidden = false; p.max = ev.total; p.value = ev.loaded;
      t.textContent = `Sending ${f.name}: ${Math.round(ev.loaded / ev.total * 100)}%`;
    }
  };
  const poll = setInterval(async () => {
    const u = await api("/api/update").catch(() => null);
    if (u && ["receiving", "verifying", "finishing"].includes(u.state.phase)) showUpdate(u.state);
  }, 1000);
  xhr.onloadend = async () => {
    clearInterval(poll);
    e.target.value = "";
    if (xhr.status !== 200) {
      let msg = xhr.statusText;
      try { msg = JSON.parse(xhr.responseText).error || msg; } catch {}
      t.textContent = "Update failed: " + msg; t.classList.add("error"); p.hidden = true;
    }
    await loadUpdate();
  };
  xhr.send(f);
});

$("#update-reboot").addEventListener("click", async () => {
  if (!confirm("Reboot into the new version now? The camera comes back in about 30 s.")) return;
  try {
    const r = await api("/api/update/reboot", {});
    $("#update-state").textContent = `Rebooting into slot ${r.rebooting.toUpperCase()}…`;
    $("#update-reboot").hidden = true;
  } catch (err) { $("#update-state").textContent = err.message; }
});

async function loadLog() {
  const r = await fetch("/api/logs");
  if (r.ok) { const p = $("#log"); p.textContent = await r.text(); p.scrollTop = p.scrollHeight; }
}
$("#log-refresh").addEventListener("click", loadLog);

async function loadCrashes() {
  const list = await api("/api/crashes").catch(() => []);
  const ul = $("#crashes");
  ul.innerHTML = list.length ? "" : "<li class=muted>None</li>";
  for (const c of list) {
    const li = document.createElement("li");
    li.append(c.name + ": ");
    for (const f of c.files) {
      const a = document.createElement("a");
      a.textContent = f + " ";
      a.onclick = async () => {
        const r = await fetch(`/api/crashes/${encodeURIComponent(c.name)}/${encodeURIComponent(f)}`);
        const p = $("#crash");
        p.hidden = false;
        p.textContent = await r.text();
        p.scrollTop = p.scrollHeight;
      };
      li.append(a);
    }
    ul.append(li);
  }
}

async function refreshStatus() {
  if (!$("#page-system").classList.contains("current")) return;
  const s = await api("/api/status").catch(() => null);
  if (!s) return;
  const m = s.media || {};
  const rows = [
    ["Version", s.version],
    ["Boot slot", s.slot.toUpperCase()],
    ["Addresses", s.addresses.map((a) => `${a.ip} (${a.if})`).join(", ") || "–"],
    ["Uptime", fmtUptime(s.system.uptime_s || 0)],
    ["SoC temperature", (s.system.soc_temp_c ?? "–") + " °C"],
    ["Load", s.system.load1 ?? "–"],
    ["Free memory", Math.round((s.system.mem_available_kb || 0) / 1024) + " MB"],
    ["Daemon memory", Math.round((s.system.rss_kb || 0) / 1024 * 10) / 10 + " MB"],
    ["YOLO", m.yolo_fps != null ? `${m.yolo_fps} fps, ${m.yolo_ms} ms` : "–"],
    ["Camera channels", m.ch0_fps != null ? `${m.ch0_fps} / ${m.ch1_fps} fps` : "–"],
  ];
  // the camera's clock, shown in the camera's time zone
  try {
    $("#clock").textContent = new Date(s.now_ms).toLocaleString(undefined, { timeZone: config.system.timezone });
  } catch { $("#clock").textContent = new Date(s.now_ms).toISOString() + " (UTC)"; }
  const t = $("#status");
  t.innerHTML = "";
  for (const [k, v] of rows) {
    const tr = t.insertRow();
    tr.insertCell().outerHTML = `<th>${k}</th>`;
    tr.insertCell().textContent = v;
  }
}
setInterval(refreshStatus, 5000);

// ---- start -------------------------------------------------------------------------------

async function reloadConfig() {
  config = await api("/api/config");
  showControls();
  await loadLabels();
  await loadModelInfo();
  loadModels();
  draw();
}

async function start() {
  let s;
  try { s = await api("/api/session"); } catch { return; }
  $("#who").textContent = s.user;
  $("#login").hidden = true;
  $("#app").hidden = false;
  await reloadConfig();
  startLive();
  route();
}

start();
