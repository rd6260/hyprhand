// hyprhand client: live WS state, tab switching, action dispatch, PWA niceties.
"use strict";

const WS_PER_MONITOR = 5;  // workspaces per monitor (must match WS_PER_MONITOR in hypr.py)
let volDragging = false;
let seekDragging = false;
let lastState = {};   // most recent WS state, for re-render after async fetches
let brightness = {};  // {monitor: pct}, fetched on demand (ddcutil is slow)
let videoCaptured = false;  // Stream "Lock input" (Pointer Lock) active -> relative input
let mainSock = null;        // the live-state WebSocket (hoisted so visibility can revive it)

async function fetchBrightness() {
  try {
    const r = await fetch("/brightness");
    if (r.ok) { brightness = await r.json(); renderWorkspaces(lastState); }
  } catch (_) {}
}

// ---- PWA: service worker, wake lock, landscape lock (secure context only) ----
if ("serviceWorker" in navigator) {
  // Root scope is load-bearing: the share-target bridge intercepts POST /share,
  // which a /static/-scoped worker can never see.
  navigator.serviceWorker.register("/sw.js").catch(() => {});
  // Evict the old /static/-scoped registration from existing installs.
  navigator.serviceWorker.getRegistrations().then((rs) =>
    rs.forEach((r) => { if (new URL(r.scope).pathname === "/static/") r.unregister(); })
  ).catch(() => {});
}

let wakeLock = null;
async function keepAwake() {
  try { wakeLock = await navigator.wakeLock?.request("screen"); } catch (_) {}
}
document.addEventListener("visibilitychange", () => {
  if (document.visibilityState !== "visible") return;
  keepAwake();
  // Backgrounded PWAs get their timers throttled, so the 2s auto-reconnect may not fire
  // while suspended — the UI can come back frozen on a dead socket. Revive on resume.
  if (!mainSock || mainSock.readyState > 1) connect();        // 2=CLOSING, 3=CLOSED
  if (!inputSock || inputSock.readyState > 1) connectInput();
});
keepAwake();
if (screen.orientation?.lock) screen.orientation.lock("landscape").catch(() => {});

// in-app fullscreen toggle (fallback when not launched as an installed PWA)
const fsBtn = document.getElementById("fs-btn");
if (fsBtn) fsBtn.onclick = () => {
  if (document.fullscreenElement) document.exitFullscreen();
  else document.documentElement.requestFullscreen().catch(() => {});
};

const reloadBtn = document.getElementById("reload-btn");
if (reloadBtn) reloadBtn.onclick = () => location.reload();

// phone battery in the top bar (Battery Status API reports the device running the
// browser = the phone). Chromium-only + secure-context; where it's unavailable the chip
// stays hidden (graceful).
(function initBattery() {
  const el = document.getElementById("st-bat");
  if (!el || !navigator.getBattery) return;
  navigator.getBattery().then((bat) => {
    const render = () => {
      const pct = Math.round(bat.level * 100);
      el.textContent = `${bat.charging ? "PWR " : "BAT "}${pct}%`;
      el.classList.toggle("text-red-400", !bat.charging && pct <= 15);
      el.classList.remove("hidden");
    };
    render();
    bat.addEventListener("levelchange", render);
    bat.addEventListener("chargingchange", render);
  }).catch(() => {});
})();

// ---- helpers ----
async function post(url, body) {
  const r = await fetch(url, {
    method: "POST", headers: { "Content-Type": "application/json" },
    body: body ? JSON.stringify(body) : "{}",
  });
  if (r.status === 401) { location.href = "/login"; return null; }
  return r.json().catch(() => ({}));
}
function setResult(id, res) {
  const el = document.getElementById(id);
  if (el && res) el.textContent = res.ok ? "✓ ok" : `✗ ${res.error || res.stderr || res.code || ""}`;
}

// ---- dock + summon (replaces the old flat tabs) ----
// The center shows Workspaces home by default; a lit dock row fills it with that panel
// (band + rail persist). Tap the lit row / a panel's back = home. Files/Config/System
// live under MORE, so opening one keeps the MORE row lit.
const PANEL_PARENT = { modes: "more", config: "more", system: "more" };
let activePanel = null;
function openPanel(pn) {
  document.querySelectorAll(".drow").forEach((x) => x.classList.remove("on"));
  document.querySelectorAll(".panel").forEach((x) => x.classList.remove("show"));
  const home = document.getElementById("home");
  if (!pn || activePanel === pn) {          // toggle the lit row -> back to home
    activePanel = null;
    home.classList.remove("hide");
    return;
  }
  activePanel = pn;
  home.classList.add("hide");
  const row = document.querySelector(`.drow[data-p="${PANEL_PARENT[pn] || pn}"]`);
  if (row) row.classList.add("on");
  const panel = document.getElementById("panel-" + pn);
  if (panel) panel.classList.add("show");
  if (pn === "modes") loadModes();
  else if (pn === "commands") loadCommands();
  else if (pn === "stream") populateMonitors();
  else if (pn === "files") { populateShotMonitors(); loadShotCameras(); }
  else if (pn === "config") loadConfig(cfgCurrent);
}
document.querySelectorAll(".drow").forEach((r) => (r.onclick = () => openPanel(r.dataset.p)));
document.querySelectorAll(".lbtn").forEach((b) => (b.onclick = () => openPanel(b.dataset.sub)));
document.querySelectorAll(".phead .back").forEach((b) => (b.onclick = () => openPanel(b.dataset.p)));

// ---- segmented controls (terminal toggles that replace native <select>) ----
function segValue(id) {
  const el = typeof id === "string" ? document.getElementById(id) : id;
  return el ? el.dataset.value : undefined;
}
function segSet(el, val) {
  if (!el) return;
  el.dataset.value = val;
  el.querySelectorAll(".segbtn").forEach((b) => b.classList.toggle("on", b.dataset.val === val));
}
function segInit(el, onChange) {
  if (!el || el._segInit) return;      // delegated listener survives innerHTML rebuilds; init once
  el._segInit = true;
  el.addEventListener("click", (e) => {
    const b = e.target.closest(".segbtn");
    if (!b || !el.contains(b) || b.dataset.val === el.dataset.value) return;
    segSet(el, b.dataset.val);
    if (onChange) onChange(b.dataset.val);
  });
}
function segRender(el, items) {         // rebuild options; keep current value if it still exists
  if (!el) return;
  const keep = items.some((it) => it.val === el.dataset.value) ? el.dataset.value : (items[0] && items[0].val);
  el.innerHTML = items.map((it) =>
    `<button type="button" class="segbtn${it.val === keep ? " on" : ""}" data-val="${it.val}">${it.label}</button>`
  ).join("");
  el.dataset.value = keep;
}

// ---- files: screenshot + drop ----
// sources = monitors (from live state) + cameras (fetched from /cameras, value "cam:<node>")
let shotCameras = [], shotMonSig = "";
function populateShotMonitors() {
  const mons = (lastState.hypr && lastState.hypr.monitors) || [];
  shotMonSig = mons.map((m) => m.name).join(",");
  const escH = (x) => String(x).replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/"/g, "&quot;");
  segRender(document.getElementById("shot-monitor"), [
    ...mons.map((m) => ({ val: m.name, label: m.name })),
    ...shotCameras.map((c) => ({ val: `cam:${escH(c.node)}`, label: escH(c.name) })),
  ]);
}
async function loadShotCameras() {
  try {
    const r = await fetch("/cameras");
    if (r.ok) shotCameras = await r.json();
  } catch (_) { shotCameras = []; }
  populateShotMonitors();
}
(function initFiles() {
  const grab = document.getElementById("shot-grab");
  if (!grab) return;
  segInit(document.getElementById("shot-monitor"));
  grab.onclick = async () => {
    const src = segValue("shot-monitor") || "";
    const isCam = src.startsWith("cam:");
    const mon = isCam ? src.slice(4) : src;
    grab.classList.add("opacity-50");
    const q = isCam ? `camera=${encodeURIComponent(mon)}` : `monitor=${encodeURIComponent(mon)}`;
    const r = await fetch(`/screenshot?${q}`);
    grab.classList.remove("opacity-50");
    if (r.status === 401) { location.href = "/login"; return; }
    if (!r.ok) { document.getElementById("up-status"); return; }
    const url = URL.createObjectURL(await r.blob());
    const img = document.getElementById("shot-img");
    img.src = url; img.classList.remove("hidden");
    const ph = document.getElementById("shot-placeholder");
    if (ph) ph.classList.add("hidden");
    const dl = document.getElementById("shot-dl");
    dl.href = url; dl.download = `${mon}.png`; dl.classList.remove("hidden");
  };

  const imgModal = document.getElementById("img-modal");
  const imgModalContent = document.getElementById("img-modal-content");
  const metaViewport = document.querySelector('meta[name="viewport"]');
  
  if (imgModal && imgModalContent) {
    const shotImg = document.getElementById("shot-img");
    if (shotImg) {
      shotImg.onclick = () => {
        imgModalContent.src = shotImg.src;
        imgModal.classList.remove("hidden");
        if (metaViewport) {
          metaViewport.setAttribute("content", "width=device-width, initial-scale=1, viewport-fit=cover, maximum-scale=5.0, user-scalable=yes");
        }
      };
    }

    imgModal.onclick = () => {
      imgModal.classList.add("hidden");
      if (metaViewport) {
        metaViewport.setAttribute("content", "width=device-width, initial-scale=1, viewport-fit=cover, maximum-scale=1.0, user-scalable=no");
      }
    };
  }

  document.getElementById("up-send").onclick = async () => {
    const f = document.getElementById("up-file").files[0];
    const status = document.getElementById("up-status");
    if (!f) { status.textContent = "pick a file first"; return; }
    status.textContent = `uploading ${f.name}…`;
    const fd = new FormData(); fd.append("file", f);
    const r = await fetch("/upload", { method: "POST", body: fd });
    if (r.status === 401) { location.href = "/login"; return; }
    const res = await r.json().catch(() => ({}));
    status.textContent = res.ok
      ? `✓ saved ${res.name} (${(res.size / 1048576).toFixed(2)} MB)`
      : `✗ ${res.error || "failed"}`;
  };

  // ---- clipboard bridge ----
  const clipRecv = document.getElementById("clip-recv");
  const clipContent = document.getElementById("clip-content");
  const clipCopy = document.getElementById("clip-copy");
  const clipSend = document.getElementById("clip-send");
  const clipText = document.getElementById("clip-text");
  const clipStatus = document.getElementById("clip-status");

  if (clipRecv) {
    clipRecv.onclick = async () => {
      clipRecv.classList.add("opacity-50");
      clipStatus.textContent = "";
      const r = await fetch("/clipboard");
      clipRecv.classList.remove("opacity-50");
      if (r.status === 401) { location.href = "/login"; return; }
      const res = await r.json().catch(() => ({}));
      if (res.ok) {
        clipContent.textContent = res.text || "(empty clipboard)";
        clipContent.classList.toggle("text-zinc-600", !res.text);
        clipContent.classList.toggle("text-zinc-300", !!res.text);
      } else {
        clipContent.textContent = `✗ ${res.error || "failed to read clipboard"}`;
        clipContent.classList.add("text-zinc-600");
      }
    };
  }

  if (clipCopy) {
    clipCopy.onclick = async () => {
      const text = clipContent.textContent;
      if (!text || text.startsWith("—") || text.startsWith("✗")) return;
      try {
        await navigator.clipboard.writeText(text);
        clipStatus.textContent = "✓ copied to phone";
      } catch {
        clipStatus.textContent = "✗ clipboard access denied";
      }
    };
  }

  if (clipSend) {
    clipSend.onclick = async () => {
      let text = clipText.value;
      if (!text) {
        try {
          text = await navigator.clipboard.readText();
        } catch {
          clipStatus.textContent = "✗ please type text or allow clipboard access";
          return;
        }
        if (!text) {
          clipStatus.textContent = "phone clipboard is empty";
          return;
        }
      }
      clipSend.classList.add("opacity-50");
      clipStatus.textContent = "sending…";
      const res = await post("/clipboard", { text });
      clipSend.classList.remove("opacity-50");
      if (res && res.ok) {
        clipStatus.textContent = "✓ sent to rig clipboard";
        clipText.value = "";
      } else {
        clipStatus.textContent = `✗ ${(res && res.error) || "failed"}`;
      }
    };
  }
})();

// ---- in-deck config editor ----
let cfgCurrent = "commands";
async function loadConfig(name) {
  cfgCurrent = name;
  document.querySelectorAll(".cfg-pick").forEach((b) =>
    b.classList.toggle("bg-emerald-700", b.dataset.cfg === name));
  document.getElementById("cfg-status").textContent = "";
  const r = await (await fetch(`/config/${name}`)).json().catch(() => ({}));
  document.getElementById("cfg-text").value = r.text || "";
}
document.querySelectorAll(".cfg-pick").forEach((b) => (b.onclick = () => loadConfig(b.dataset.cfg)));
const cfgSave = document.getElementById("cfg-save");
if (cfgSave) cfgSave.onclick = async () => {
  const status = document.getElementById("cfg-status");
  status.textContent = "saving…"; status.className = "text-xs flex-1 text-zinc-400";
  const res = await post(`/config/${cfgCurrent}`, { text: document.getElementById("cfg-text").value });
  if (res && res.ok) {
    status.textContent = "✓ saved"; status.className = "text-xs flex-1 text-emerald-400";
    if (cfgCurrent === "context") loadContextConfig();   // strip reflects edits without a reload
  } else { status.textContent = `✗ ${res ? res.error : "failed"}`; status.className = "text-xs flex-1 text-red-400"; }
};



// ---- audio / media ----
document.querySelectorAll("[data-mute]").forEach((b) =>
  (b.onclick = () => post("/audio/mute", { target: b.dataset.mute })));
document.querySelectorAll(".media").forEach((b) =>
  (b.onclick = () => post(`/media/${b.dataset.media}`)));
// volume: multiple sliders stay in sync (one in the rail, a redundant one in the Audio panel).
// Every mute button/glyph (incl. the rail one) is already wired by the [data-mute] handler above.
function setVolText(v) { document.querySelectorAll(".vol-val").forEach((el) => (el.textContent = v)); }
document.querySelectorAll(".vol-slider").forEach((sl) => {
  sl.addEventListener("pointerdown", () => (volDragging = true));
  sl.addEventListener("input", () => {
    setVolText(sl.value);
    document.querySelectorAll(".vol-slider").forEach((o) => { if (o !== sl) o.value = sl.value; });
  });
  sl.addEventListener("change", () => { post("/audio/volume", { pct: Number(sl.value) }); volDragging = false; });
});

// seek bar — only in the Audio panel; shows position within current track.
function fmtTime(us) {
  if (us == null || us < 0) return "—";
  const s = Math.floor(us / 1_000_000);
  const m = Math.floor(s / 60);
  return `${m}:${String(s % 60).padStart(2, "0")}`;
}
const seekSlider = document.getElementById("seek-slider");
const seekWrap   = document.getElementById("seek-wrap");
const seekElapsed = document.getElementById("seek-elapsed");
const seekTotal   = document.getElementById("seek-total");
if (seekSlider) {
  seekSlider.addEventListener("pointerdown", () => (seekDragging = true));
  seekSlider.addEventListener("input", () => {
    // show live elapsed time while scrubbing (dur_us stored on element)
    const durUs = Number(seekSlider.dataset.durUs || 0);
    const posUs = (Number(seekSlider.value) / 1000) * durUs;
    if (seekElapsed) seekElapsed.textContent = fmtTime(posUs);
  });
  seekSlider.addEventListener("change", () => {
    const durUs = Number(seekSlider.dataset.durUs || 0);
    const posUs = Math.round((Number(seekSlider.value) / 1000) * durUs);
    post("/media/seek", { position_us: posUs });
    seekDragging = false;
  });
}

// ---- system: lock + kill ----
document.querySelectorAll("[data-sys]").forEach((b) => {
  if (b.dataset.sys === "lock") b.onclick = () => post("/sys/lock");
});

// ---- commands: buttons that run shell commands on the rig + manage (CRUD) ----
// Stored in hyprhand.json `commands: [{label, cmd}]`; ids are list indices, so every
// mutation reloads the list from the server.
let cmdList = [];
let cmdManaging = false;
const htmlEsc = (s) => String(s).replace(/[&<>"']/g, (c) =>
  ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));

function cmdStatus(text, cls = "text-zinc-400") {
  const el = document.getElementById("cmd-status");
  el.textContent = text;
  el.className = `sub flex-1 truncate ${cls}`;
}

async function loadCommands() {
  const r = await fetch("/commands");
  if (r.status === 401) { location.href = "/login"; return; }
  cmdList = await r.json().catch(() => []);
  renderCommands();
}

function renderCommands() {
  const grid = document.getElementById("cmd-grid");
  grid.innerHTML = cmdList.length
    ? cmdList.map((c) =>
        `<button class="cmd-btn bg-zinc-800 rounded-lg py-5 px-2 text-sm font-medium break-words" data-id="${c.id}" title="${htmlEsc(c.cmd)}">${htmlEsc(c.label)}</button>`).join("")
    : '<p class="col-span-3 text-zinc-600 text-sm">No commands yet — tap <b>Manage</b> to add one.</p>';

  const list = document.getElementById("cmd-list");
  list.innerHTML = cmdList.map((c, i) => `
    <div class="bg-zinc-900 rounded-lg p-2 flex items-center gap-2" data-id="${c.id}">
      <div class="flex-1 min-w-0">
        <div class="text-sm font-medium truncate">${htmlEsc(c.label)}</div>
        <div class="text-xs text-zinc-500 font-mono truncate">${htmlEsc(c.cmd)}</div>
      </div>
      <button class="cmd-act bg-zinc-800 rounded px-2 py-1 text-xs" data-act="up" ${i === 0 ? "disabled style='opacity:.3'" : ""}>↑</button>
      <button class="cmd-act bg-zinc-800 rounded px-2 py-1 text-xs" data-act="down" ${i === cmdList.length - 1 ? "disabled style='opacity:.3'" : ""}>↓</button>
      <button class="cmd-act bg-zinc-800 rounded px-3 py-1 text-xs" data-act="edit">Edit</button>
      <button class="cmd-act bg-red-900 rounded px-3 py-1 text-xs" data-act="del">Delete</button>
    </div>`).join("");
}

function cmdResetForm() {
  document.getElementById("cmd-id").value = "";
  document.getElementById("cmd-label").value = "";
  document.getElementById("cmd-cmd").value = "";
  document.getElementById("cmd-submit").textContent = "Add";
  document.getElementById("cmd-cancel").classList.add("hidden");
  document.getElementById("cmd-form-status").textContent = "";
}

(function initCommands() {
  const grid = document.getElementById("cmd-grid");
  if (!grid) return;

  // run
  grid.addEventListener("click", async (ev) => {
    const b = ev.target.closest(".cmd-btn");
    if (!b || b.disabled) return;
    const label = b.textContent;
    b.disabled = true; b.classList.add("opacity-50");
    cmdStatus(`running ${label}…`);
    const res = await post("/commands/run", { id: Number(b.dataset.id) });
    b.disabled = false; b.classList.remove("opacity-50");
    if (res && res.ok) cmdStatus(`✓ ${label}${res.status === "started" ? " (started)" : ""}`, "text-emerald-400");
    else cmdStatus(`✗ ${label}: ${(res && res.error) || "failed"}`, "text-red-400");
  });

  // manage toggle
  document.getElementById("cmd-manage").onclick = () => {
    cmdManaging = !cmdManaging;
    document.getElementById("cmd-run").classList.toggle("hidden", cmdManaging);
    document.getElementById("cmd-admin").classList.toggle("hidden", !cmdManaging);
    document.getElementById("cmd-manage").textContent = cmdManaging ? "Done" : "Manage";
    document.getElementById("cmd-manage").classList.toggle("bg-emerald-700", cmdManaging);
    cmdResetForm();
    cmdStatus("");
  };

  // create / update
  document.getElementById("cmd-form").onsubmit = async (ev) => {
    ev.preventDefault();
    const st = document.getElementById("cmd-form-status");
    const idv = document.getElementById("cmd-id").value;
    const body = {
      label: document.getElementById("cmd-label").value,
      cmd: document.getElementById("cmd-cmd").value,
    };
    if (idv !== "") body.id = Number(idv);
    st.textContent = "saving…"; st.className = "text-xs flex-1 text-zinc-400";
    const res = await post("/commands/save", body);
    if (res && res.ok) {
      cmdResetForm();
      await loadCommands();
      st.textContent = "✓ saved"; st.className = "text-xs flex-1 text-emerald-400";
    } else {
      st.textContent = `✗ ${(res && res.error) || "failed"}`; st.className = "text-xs flex-1 text-red-400";
    }
  };
  document.getElementById("cmd-cancel").onclick = cmdResetForm;

  // edit / delete / reorder
  document.getElementById("cmd-list").addEventListener("click", async (ev) => {
    const b = ev.target.closest(".cmd-act");
    if (!b || b.disabled) return;
    const id = Number(b.closest("[data-id]").dataset.id);
    const c = cmdList.find((x) => x.id === id);
    if (!c) return;
    const act = b.dataset.act;
    if (act === "edit") {
      document.getElementById("cmd-id").value = String(id);
      document.getElementById("cmd-label").value = c.label;
      document.getElementById("cmd-cmd").value = c.cmd;
      document.getElementById("cmd-submit").textContent = "Save";
      document.getElementById("cmd-cancel").classList.remove("hidden");
      document.getElementById("cmd-form-status").textContent = `editing “${c.label}”`;
      document.getElementById("cmd-label").focus();
      return;
    }
    let res;
    if (act === "del") {
      if (!confirm(`Delete “${c.label}”?`)) return;
      res = await post("/commands/delete", { id });
      if (document.getElementById("cmd-id").value !== "") cmdResetForm(); // ids shifted
    } else {
      res = await post("/commands/move", { id, dir: act === "up" ? -1 : 1 });
      if (document.getElementById("cmd-id").value !== "") cmdResetForm();
    }
    if (!res || !res.ok) cmdStatus(`✗ ${(res && res.error) || "failed"}`, "text-red-400");
    await loadCommands();
  });
})();

// ---- remote input (keyboard + trackpad over a dedicated WS) ----
let inputSock = null;
function connectInput() {
  if (inputSock && inputSock.readyState <= 1) return;   // already open/connecting — no dupes
  const proto = location.protocol === "https:" ? "wss" : "ws";
  inputSock = new WebSocket(`${proto}://${location.host}/ws/input`);
  inputSock.onclose = async () => {
    if (!(await authOk())) { location.href = "/login"; return; }
    setTimeout(connectInput, 2000);
  };
}
function sendInput(o) {
  if (inputSock && inputSock.readyState === 1) inputSock.send(JSON.stringify(o));
}

(function initRemote() {
  const tp = document.getElementById("trackpad");
  if (!tp) return;
  const SENS = 3.0, ACCEL = 0.06;  // base gain + acceleration (faster swipe = farther)
  // Pointer Events so the trackpad works from touch AND a laptop mouse. The gesture
  // logic mirrors the old touch handlers exactly: 1 pointer = move/tap-left, 2 pointers
  // = scroll/tap-right; plus a wheel handler for mouse scroll.
  const pts = new Map();   // pointerId -> {x, y}
  let moved = false, startT = 0, wasTwo = false, scrollY = null;

  tp.addEventListener("pointerdown", (ev) => {
    ev.preventDefault();
    tp.setPointerCapture?.(ev.pointerId);
    pts.set(ev.pointerId, { x: ev.clientX, y: ev.clientY });
    if (pts.size === 1) { moved = false; startT = Date.now(); wasTwo = false; }
    else if (pts.size === 2) {
      wasTwo = true;
      const a = [...pts.values()];
      scrollY = (a[0].y + a[1].y) / 2;
    }
  });

  tp.addEventListener("pointermove", (ev) => {
    const prev = pts.get(ev.pointerId);
    if (!prev) return;
    ev.preventDefault();
    pts.set(ev.pointerId, { x: ev.clientX, y: ev.clientY });
    if (pts.size >= 2) {
      const a = [...pts.values()];
      const y = (a[0].y + a[1].y) / 2;
      if (scrollY != null) {
        const d = y - scrollY;
        // natural scrolling: content follows the fingers (swipe up -> scroll down)
        if (Math.abs(d) > 3) { sendInput({ t: "scroll", dy: d < 0 ? -1 : 1 }); scrollY = y; }
      }
      return;
    }
    const dx = ev.clientX - prev.x, dy = ev.clientY - prev.y;
    if (Math.abs(dx) + Math.abs(dy) > 2) moved = true;
    const f = SENS * (1 + Math.hypot(dx, dy) * ACCEL);
    sendInput({ t: "move", dx: Math.round(dx * f), dy: Math.round(dy * f) });
  });

  function tpUp(ev) {
    if (!pts.has(ev.pointerId)) return;
    pts.delete(ev.pointerId);
    if (pts.size > 0) return;                 // wait for the last pointer to lift
    const quick = Date.now() - startT < (wasTwo ? 300 : 250);
    if (!moved && quick) sendInput({ t: "click", b: wasTwo ? "right" : "left" });
    wasTwo = false; scrollY = null;
  }
  tp.addEventListener("pointerup", tpUp);
  tp.addEventListener("pointercancel", tpUp);
  tp.addEventListener("wheel", (ev) => {        // mouse wheel -> scroll
    ev.preventDefault();
    sendInput({ t: "scroll", dy: ev.deltaY < 0 ? 1 : -1 });
  }, { passive: false });

  document.querySelectorAll("[data-click]").forEach((b) =>
    (b.onclick = () => sendInput({ t: "click", b: b.dataset.click })));

  // ── swipeable pane: trackpad (0) ↔ keyboard (1) ──
  const swiper = document.getElementById("remote-swiper");
  const track = document.getElementById("remote-track");
  const PEEK = 20; // px of the adjacent pane visible at the edge
  let pane = 0, swStartX = 0, swStartY = 0, swiping = false, swMoved = false;

  function paneOffset(n) {
    return n === 0
      ? `translateX(0px)`
      : `translateX(calc(-50% + ${PEEK * 2}px))`;
  }
  function setPane(n) {
    pane = n;
    track.style.transform = paneOffset(n);
  }
  setPane(0); // initialize with peek visible

  // swipe via touch (separate from pointer events so it doesn't interfere with trackpad)
  swiper.addEventListener("touchstart", (ev) => {
    // only initiate swipe from a single touch; ignore if starting on the trackpad surface
    if (ev.touches.length !== 1) return;
    const t = ev.touches[0];
    // don't swipe-intercept touches that start on the trackpad itself
    if (tp.contains(ev.target)) return;
    swStartX = t.clientX; swStartY = t.clientY; swiping = true; swMoved = false;
    track.classList.add("no-anim");
  }, { passive: true });

  swiper.addEventListener("touchmove", (ev) => {
    if (!swiping) return;
    const dx = ev.touches[0].clientX - swStartX;
    const dy = ev.touches[0].clientY - swStartY;
    // if vertical motion dominates, bail (let it scroll the keyboard area)
    if (!swMoved && Math.abs(dy) > Math.abs(dx) + 5) { swiping = false; track.classList.remove("no-anim"); return; }
    if (Math.abs(dx) > 10) swMoved = true;
    if (!swMoved) return;
    ev.preventDefault();
    const W = swiper.offsetWidth;
    const basePx = pane === 0 ? 0 : -(W - PEEK * 2);
    const newPx = Math.max(-(W - PEEK * 2), Math.min(0, basePx + dx));
    track.style.transform = `translateX(${newPx}px)`;
  }, { passive: false });

  swiper.addEventListener("touchend", (ev) => {
    if (!swiping) return;
    swiping = false;
    track.classList.remove("no-anim");
    if (!swMoved) { setPane(pane); return; }
    const dx = ev.changedTouches[0].clientX - swStartX;
    if (dx < -50 && pane < 1) setPane(1);
    else if (dx > 50 && pane > 0) setPane(0);
    else setPane(pane); // snap back
  }, { passive: true });

  // ── haptic feedback helper ──
  function haptic() { if (navigator.vibrate) navigator.vibrate(12); }

  // ── full keyboard: modifier state + key dispatch ──
  const mods = { ctrl: false, alt: false, shift: false, super: false };
  let capsLock = false;

  function isUpperCase() { return mods.shift !== capsLock; } // XOR — shift flips capslock state

  function updateModUI() {
    document.querySelectorAll("[data-modifier]").forEach((b) => {
      b.classList.toggle("mod-active", mods[b.dataset.modifier]);
    });
    // capslock visual
    document.querySelectorAll("[data-mod-toggle='caps']").forEach((b) => {
      b.classList.toggle("mod-active", capsLock);
    });
  }

  function updateKeyLabels() {
    const upper = isUpperCase();
    document.querySelectorAll("#kb-area [data-char]").forEach((k) => {
      const ch = k.dataset.char;
      const sh = k.dataset.shift;
      if (ch.length === 1) {
        // letter keys: toggle case
        if (ch >= "a" && ch <= "z") {
          k.textContent = upper ? ch.toUpperCase() : ch;
        }
        // symbol/number keys with data-shift: show shifted symbol when shift is active (not caps)
        else if (sh && mods.shift) {
          k.textContent = sh;
        } else {
          k.textContent = ch;
        }
      }
    });
  }

  function clearMods() {
    mods.ctrl = false; mods.alt = false; mods.shift = false; mods.super = false;
    updateModUI();
    updateKeyLabels();
  }

  function getActiveMods() {
    const m = [];
    if (mods.ctrl) m.push("ctrl");
    if (mods.alt) m.push("alt");
    if (mods.shift) m.push("shift");
    if (mods.super) m.push("super");
    return m;
  }

  // ── Typed text tracker ──
  let typedText = "";
  let typedTextTimeout = null;
  function updateTypedText(s, isSpecial) {
    const el = document.getElementById("kb-typed-text");
    if (!el) return;
    
    if (isSpecial) {
      if (s === "backspace") {
        typedText = typedText.slice(0, -1);
      } else if (s === "enter") {
        typedText += " ";
      } else if (s === "tab") {
        typedText += "    ";
      }
    } else {
      typedText += s;
    }
    
    el.textContent = typedText;
    el.scrollLeft = el.scrollWidth;
    
    if (typedTextTimeout) clearTimeout(typedTextTimeout);
    typedTextTimeout = setTimeout(() => {
      typedText = "";
      el.textContent = "";
    }, 30000);
  }

  // Handle keyboard key taps
  document.getElementById("kb-area").addEventListener("click", (ev) => {
    const key = ev.target.closest(".kb-key");
    if (!key) return;
    haptic();

    // ── Modifier toggle ──
    const mod = key.dataset.modifier;
    if (mod) {
      mods[mod] = !mods[mod];
      updateModUI();
      updateKeyLabels();
      return;
    }

    // ── CapsLock toggle ──
    if (key.dataset.modToggle === "caps") {
      capsLock = !capsLock;
      sendInput({ t: "combo", keys: ["capslock"] });
      updateModUI();
      updateKeyLabels();
      clearMods(); // clear shift etc after capslock
      return;
    }

    // ── Special key ──
    const special = key.dataset.special;
    if (special) {
      const am = getActiveMods();
      if (am.length > 0) {
        sendInput({ t: "combo", keys: [...am, special] });
      } else {
        sendInput({ t: "combo", keys: [special] });
      }
      updateTypedText(special, true);
      clearMods();
      return;
    }

    // ── Character key ──
    const ch = key.dataset.char;
    if (ch !== undefined) {
      const am = getActiveMods();
      // Determine which character to send
      let toSend = ch;
      if (ch >= "a" && ch <= "z") {
        // letter: apply capslock + shift (XOR)
        toSend = isUpperCase() ? ch.toUpperCase() : ch;
      } else if (mods.shift && key.dataset.shift) {
        // number/symbol row: shifted symbol
        toSend = key.dataset.shift;
      }
      // If non-shift modifiers are active, send as combo: all held mods (incl. shift)
      // + the unshifted base key, so ctrl+shift+o arrives as ctrl+shift+o, not ctrl+o.
      const nonShiftMods = am.filter((m) => m !== "shift");
      if (nonShiftMods.length > 0) {
        sendInput({ t: "combo", keys: [...am, ch] });
      } else {
        sendInput({ t: "text", text: toSend });
      }
      updateTypedText(toSend, false);
      clearMods();
      return;
    }
  });

  // flash key on tap for visual feedback
  document.getElementById("kb-area").addEventListener("pointerdown", (ev) => {
    const key = ev.target.closest(".kb-key");
    if (key && !key.dataset.modifier && key.dataset.modToggle !== "caps") {
      key.classList.add("pressed");
      const rm = () => { key.classList.remove("pressed"); key.removeEventListener("pointerup", rm); key.removeEventListener("pointerleave", rm); };
      key.addEventListener("pointerup", rm);
      key.addEventListener("pointerleave", rm);
    }
  });

  fetch("/input/available").then((r) => r.json()).then((r) => {
    if (!r.ok) tp.textContent = "Remote input unavailable (no /dev/uinput access)";
  }).catch(() => {});
})();

// ---- audio streaming (WebRTC, bidirectional) ----
let streamPc = null, micStream = null;
const streamOpts = { listen: true, mic: false, phoneOnly: false, video: false };
function streamState(text, on) {
  document.getElementById("stream-status").textContent = text;
  document.getElementById("stream-dot").classList.toggle("bg-emerald-500", !!on);
  document.getElementById("stream-dot").classList.toggle("bg-zinc-600", !on);
}
function populateMonitors() {
  const mons = (lastState.hypr && lastState.hypr.monitors) || [];
  segRender(document.getElementById("stream-monitor"), mons.map((m) => ({ val: m.name, label: m.name })));
}
async function streamStop() {
  if (streamPc) { streamPc.close(); streamPc = null; }
  if (micStream) { micStream.getTracks().forEach((t) => t.stop()); micStream = null; }
  document.getElementById("stream-audio").srcObject = null;
  const v = document.getElementById("stream-video-el");
  v.srcObject = null; v.classList.add("hidden");
  await post("/audio/webrtc/stop");
  streamState("Idle", false);
}
async function streamConnect() {
  if (!streamOpts.listen && !streamOpts.mic && !streamOpts.video) {
    streamState("enable a direction first", false); return;
  }
  await streamStop();
  streamState("connecting…", false);
  const pc = new RTCPeerConnection();
  streamPc = pc;
  pc.ontrack = (e) => {
    if (e.track.kind === "video") {
      const v = document.getElementById("stream-video-el");
      v.srcObject = e.streams[0]; v.classList.remove("hidden");
    } else {
      document.getElementById("stream-audio").srcObject = e.streams[0];
    }
  };
  pc.onconnectionstatechange = () => {
    if (pc !== streamPc) return;
    if (pc.connectionState === "connected") streamState("● connected", true);
    else if (["failed", "disconnected", "closed"].includes(pc.connectionState)) streamState(pc.connectionState, false);
  };
  try {
    if (streamOpts.mic) {
      micStream = await navigator.mediaDevices.getUserMedia({
        audio: { echoCancellation: true, noiseSuppression: true, autoGainControl: true } });
      // one bidirectional m-line when also listening, so the rig-audio track can't land on
      // the wrong transceiver (aiortc.addTrack reuses the first audio transceiver it finds)
      const dir = streamOpts.listen ? "sendrecv" : "sendonly";
      micStream.getAudioTracks().forEach((t) => pc.addTransceiver(t, { direction: dir }));
    } else if (streamOpts.listen) {
      pc.addTransceiver("audio", { direction: "recvonly" });
    }
    if (streamOpts.video) pc.addTransceiver("video", { direction: "recvonly" });
    await pc.setLocalDescription(await pc.createOffer());
    await new Promise((res) => {                    // non-trickle: wait for ICE gather
      if (pc.iceGatheringState === "complete") return res();
      pc.onicegatheringstatechange = () => pc.iceGatheringState === "complete" && res();
    });
    const ans = await post("/audio/webrtc/offer", {
      sdp: pc.localDescription.sdp, type: pc.localDescription.type,
      listen: streamOpts.listen, mic: streamOpts.mic, phoneOnly: streamOpts.phoneOnly,
      video: streamOpts.video ? segValue("stream-monitor") : null,
      videoQuality: segValue("stream-quality") });
    if (!ans || !ans.sdp) { streamState("offer rejected", false); return; }
    await pc.setRemoteDescription(ans);
  } catch (err) {
    streamState("error: " + (err.name || err), false);
    await streamStop();
  }
}
(function initStream() {
  const c = document.getElementById("stream-connect");
  if (!c) return;
  c.onclick = streamConnect;
  document.getElementById("stream-stop").onclick = streamStop;
  const toggle = (id, key) => {
    const b = document.getElementById(id);
    b.classList.toggle("on", !!streamOpts[key]);          // reflect initial state
    b.onclick = () => {
      streamOpts[key] = !streamOpts[key];
      b.classList.toggle("on", streamOpts[key]);
    };
  };
  toggle("stream-listen", "listen");
  toggle("stream-mic", "mic");
  toggle("stream-phoneonly", "phoneOnly");
  toggle("stream-video", "video");
  segInit(document.getElementById("stream-monitor"));
  segInit(document.getElementById("stream-quality"));

  // touch-on-video -> remote cursor. Pointer events cover touch AND mouse (laptops).
  // tap = left click · drag = move-with-button · long-press = right click.
  const vid = document.getElementById("stream-video-el");
  const vmon = () => segValue("stream-monitor");
  function vfrac(ev) {                       // letterbox-correct fraction within the video content
    if (!vid.videoWidth) return null;
    const r = vid.getBoundingClientRect();
    const s = Math.min(r.width / vid.videoWidth, r.height / vid.videoHeight);
    const dw = vid.videoWidth * s, dh = vid.videoHeight * s;
    const fx = (ev.clientX - r.left - (r.width - dw) / 2) / dw;
    const fy = (ev.clientY - r.top - (r.height - dh) / 2) / dh;
    return (fx < 0 || fx > 1 || fy < 0 || fy > 1) ? null : { fx, fy };
  }
  function vsend(t, ev) { const p = vfrac(ev); if (p) sendInput({ t, monitor: vmon(), fx: p.fx, fy: p.fy }); }
  let vDown = false, vMoved = false, vDrag = false, vStart = null, vTimer = null, vLast = 0;
  vid.addEventListener("pointerdown", (ev) => {
    if (!vid.srcObject || videoCaptured) return;   // capture mode uses relative input instead
    ev.preventDefault(); vid.setPointerCapture(ev.pointerId);
    vDown = true; vMoved = false; vDrag = false; vStart = { x: ev.clientX, y: ev.clientY };
    vsend("vmove", ev);
    vTimer = setTimeout(() => { if (vDown && !vMoved) { vsend("vrclick", ev); vDown = false; } }, 500);
  });
  vid.addEventListener("pointermove", (ev) => {
    if (!vDown || videoCaptured) return;
    if (!vMoved && Math.hypot(ev.clientX - vStart.x, ev.clientY - vStart.y) > 6) {
      vMoved = true; vDrag = true; clearTimeout(vTimer); vsend("vdown", ev);
    }
    if (vMoved) { const now = performance.now(); if (now - vLast > 40) { vLast = now; vsend("vmove", ev); } }
  });
  function vEnd(ev) {
    clearTimeout(vTimer);
    if (!vDown) return;
    if (vDrag) sendInput({ t: "vup" });        // release always, no coords needed
    else if (!vMoved) vsend("vclick", ev);
    vDown = false; vDrag = false; vMoved = false;
  }
  vid.addEventListener("pointerup", vEnd);
  vid.addEventListener("pointercancel", vEnd);
})();

// ---- capture mode: "Lock input" on Stream (laptop becomes kb+mouse+screen) ----
(function initCapture() {
  const btn = document.getElementById("stream-capture");
  const vid = document.getElementById("stream-video-el");
  if (!btn || !vid) return;
  const SPECIAL = {
    Enter: "enter", Escape: "esc", Tab: "tab", Backspace: "backspace", Delete: "delete",
    ArrowUp: "up", ArrowDown: "down", ArrowLeft: "left", ArrowRight: "right",
    Home: "home", End: "end", PageUp: "pageup", PageDown: "pagedown", " ": "space",
  };

  btn.onclick = () => {
    if (!vid.srcObject) { document.getElementById("stream-status").textContent = "turn video on first"; return; }
    vid.requestPointerLock?.();
  };
  document.addEventListener("pointerlockchange", () => {
    videoCaptured = document.pointerLockElement === vid;
    btn.textContent = videoCaptured ? "LOCKED — Esc to release" : "LOCK input → rig (mouse + keys)";
    btn.classList.toggle("bg-emerald-700", videoCaptured);
    btn.classList.toggle("text-zinc-400", !videoCaptured);
    btn.classList.toggle("bg-zinc-800", !videoCaptured);
  });

  // relative mouse + clicks + wheel while captured (reuses the existing hid vocab)
  vid.addEventListener("mousemove", (ev) => {
    if (videoCaptured && (ev.movementX || ev.movementY))
      sendInput({ t: "move", dx: ev.movementX, dy: ev.movementY });
  });
  vid.addEventListener("mousedown", (ev) => {
    if (!videoCaptured) return;
    ev.preventDefault();
    sendInput({ t: "click", b: ev.button === 2 ? "right" : ev.button === 1 ? "middle" : "left" });
  });
  vid.addEventListener("contextmenu", (ev) => { if (videoCaptured) ev.preventDefault(); });
  vid.addEventListener("wheel", (ev) => {
    if (!videoCaptured) return;
    ev.preventDefault();
    sendInput({ t: "scroll", dy: ev.deltaY < 0 ? 1 : -1 });
  }, { passive: false });

  // keyboard forward while captured. Esc can't be sent — it releases Pointer Lock.
  window.addEventListener("keydown", (ev) => {
    if (!videoCaptured) return;
    ev.preventDefault();
    const k = ev.key;
    const mods = [];
    if (ev.ctrlKey) mods.push("ctrl");
    if (ev.altKey) mods.push("alt");
    if (ev.metaKey) mods.push("super");
    if (mods.length) {                                  // chord -> combo
      let base = SPECIAL[k] || (k.length === 1 ? k.toLowerCase() : null);
      if (ev.shiftKey) mods.push("shift");
      if (base) sendInput({ t: "combo", keys: [...mods, base] });
    } else if (SPECIAL[k]) {                            // named key (+ optional shift)
      sendInput({ t: "combo", keys: [...(ev.shiftKey ? ["shift"] : []), SPECIAL[k]] });
    } else if (k.length === 1) {                        // printable -> text (handles case)
      sendInput({ t: "text", text: k });
    }
  });
})();

// ---- modes / scenes ----
async function loadModes() {
  const grid = document.getElementById("mode-grid");
  const list = await (await fetch("/modes")).json().catch(() => []);
  if (!list.length) {
    grid.innerHTML = '<p class="col-span-3 text-zinc-600 text-sm">No modes. Add them to ~/.config/hyprhand/modes.json</p>';
    return;
  }
  grid.innerHTML = list.map((m) =>
    `<button class="mode bg-zinc-800 rounded-lg py-6 text-lg font-medium" data-id="${m.id}">${m.label}</button>`).join("");
  grid.querySelectorAll(".mode").forEach((b) => {
    b.onclick = async () => {
      const out = document.getElementById("mode-output");
      out.textContent = `applying ${b.textContent}…`;
      b.classList.add("opacity-50");
      const res = await post("/modes/run", { id: b.dataset.id });
      b.classList.remove("opacity-50");
      out.textContent = res.ok ? (res.steps || []).join("\n") : `✗ ${res.error || ""}`;
    };
  });
}



// ---- live state rendering ----
// Band telemetry readouts. Each readout self-colors by its worst signal: amber at
// temp >=80 or util >=90, red at temp >=88. Tap the clock for full gauges (cogitator).
function renderStatus(s) {
  const g = s.telemetry?.gpu, c = s.telemetry?.cpu, a = s.audio;
  const heat = (temp, util) =>
    (temp >= 88) ? "alert" : (temp >= 80 || util >= 90) ? "warn" : "";
  const paint = (el, cls) => {
    if (!el) return;
    el.classList.toggle("warn", cls === "warn");
    el.classList.toggle("alert", cls === "alert");
  };
  const gpuEl = document.getElementById("st-gpu");
  if (g && gpuEl) {
    gpuEl.textContent =
      `GPU ${g.util | 0}% · ${(g.vram_used / 1024).toFixed(1)}/${(g.vram_total / 1024).toFixed(0)}G · ${g.temp | 0}°`;
    paint(gpuEl, heat(g.temp ?? 0, g.util ?? 0));
  }
  const cpuEl = document.getElementById("st-cpu");
  if (c && cpuEl) {
    cpuEl.textContent =
      `CPU ${c.util | 0}%${c.temp ? " · " + (c.temp | 0) + "°" : ""} · ${c.mem_used}/${c.mem_total}G`;
    paint(cpuEl, heat(c.temp ?? 0, c.util ?? 0));
  }
  const audEl = document.getElementById("st-aud");
  if (a && audEl) audEl.textContent =
    `${a.mic_muted ? "MIC×" : "MIC"} · ${a.sink_muted ? "MUTE" : (a.volume ?? "—") + "%"}`;
}

function renderWorkspaces(s) {
  const grid = document.getElementById("wsgrid");
  const mons = s.hypr?.monitors || [];
  // Column count tracks the monitor count (capped at 3), so a single-monitor laptop
  // gets a full-width card instead of one stranded in the left third of the screen.
  grid.style.gridTemplateColumns = `repeat(${Math.min(mons.length || 1, 3)}, minmax(0, 1fr))`;
  // On a single wide card, cap the workspace buttons (aspect-square would otherwise
  // balloon each to width/5 -> 3 rows of giant squares that overflow the page). Small
  // fixed squares that flow. Multi-monitor cards are narrow, so keep the clean 5-up.
  const wsCols = mons.length <= 1
    ? "repeat(auto-fit, minmax(2.75rem, 3rem))"
    : `repeat(${WS_PER_MONITOR}, minmax(0, 1fr))`;
  grid.innerHTML = mons.map((m, mi) => `
    <div class="bg-zinc-900 rounded-xl p-3 flex flex-col gap-2 min-h-0 overflow-hidden">
      <div class="text-xs text-zinc-500 truncate shrink-0">${m.name} · ${m.model || ""} · ${m.refresh}Hz
        ${m.focused ? '<span class="text-emerald-500">●</span>' : ""}</div>
      <div class="grid gap-1.5 content-start shrink-0" style="grid-template-columns:${wsCols}">
        ${Array.from({length: WS_PER_MONITOR}, (_, i) => mi * WS_PER_MONITOR + i + 1).map((n) => `
          <button class="ws aspect-square rounded-lg text-sm font-medium
            ${m.active_ws === n ? "bg-emerald-600" : "bg-zinc-800"}"
            data-ws="${n}">${n}</button>`).join("")}
      </div>
      <div class="flex items-center gap-2 shrink-0">
        <button class="dpms text-xs px-2 py-1 rounded ${m.dpms ? "bg-zinc-800" : "bg-red-800"}"
          data-mon="${m.name}" data-on="${m.dpms ? 1 : 0}" title="DPMS">${m.dpms ? "● on" : "○ off"}</button>
        <span class="text-xs text-emerald-500">BRT</span>
        <input type="range" min="0" max="100" value="${brightness[m.name] ?? 50}"
          class="bri flex-1" data-mon="${m.name}" ${m.name in brightness ? "" : "disabled"}>
      </div>
      <div class="space-y-1 overflow-auto flex-1 min-h-0">
        ${(s.hypr?.windows || []).filter((w) => w.monitor === m.name).map((w) => `
          <button class="win w-full text-left text-xs rounded px-2 py-1 truncate
            ${w.focused ? "bg-emerald-700" : "bg-zinc-800"}" data-addr="${w.address}"
            oncontextmenu="return false"
            >${w.ws != null ? w.ws + " · " : ""}${(w.title || w.cls || "?").replace(/</g, "&lt;")}</button>`).join("")}
      </div>
    </div>`).join("");
  grid.querySelectorAll(".ws").forEach((b) => {
    b.onclick = () => post("/hypr/workspace", { id: Number(b.dataset.ws) });
  });
  grid.querySelectorAll(".dpms").forEach((b) => {
    b.onclick = () => post("/hypr/dpms", { monitor: b.dataset.mon, on: b.dataset.on !== "1" });
  });
  grid.querySelectorAll(".bri").forEach((s) => {
    s.addEventListener("change", () => {
      brightness[s.dataset.mon] = Number(s.value);
      post("/brightness", { monitor: s.dataset.mon, pct: Number(s.value) });
    });
  });

  let dragState = null;
  grid.querySelectorAll(".win").forEach((b) => {
    const handleStart = (e) => {
      if (e.type === "mousedown" && e.button !== 0) return;
      if (dragState) return;

      const isTouch = e.type === "touchstart";
      const clientX = isTouch ? e.touches[0].clientX : e.clientX;
      const clientY = isTouch ? e.touches[0].clientY : e.clientY;

      const addr = b.dataset.addr;
      dragState = {
        addr,
        startX: clientX,
        startY: clientY,
        isDragging: false,
        timer: setTimeout(() => {
          dragState.isDragging = true;
          const rect = b.getBoundingClientRect();
          const ghost = b.cloneNode(true);
          ghost.style.position = "fixed";
          ghost.style.left = rect.left + "px";
          ghost.style.top = rect.top + "px";
          ghost.style.width = rect.width + "px";
          ghost.style.height = rect.height + "px";
          ghost.style.opacity = "0.8";
          ghost.style.zIndex = "9999";
          ghost.style.pointerEvents = "none";
          ghost.style.transform = "scale(1.05)";
          ghost.style.transition = "transform 0.1s";
          document.body.appendChild(ghost);
          dragState.ghost = ghost;
          if (navigator.vibrate) navigator.vibrate(50);
        }, 500)
      };

      const onMove = (me) => {
        if (!dragState) return;
        const mx = me.type === "touchmove" ? me.touches[0].clientX : me.clientX;
        const my = me.type === "touchmove" ? me.touches[0].clientY : me.clientY;

        if (!dragState.isDragging) {
          if (Math.hypot(mx - dragState.startX, my - dragState.startY) > 10) {
            clearTimeout(dragState.timer);
            cleanup();
          }
        } else {
          if (me.cancelable) me.preventDefault();
          dragState.ghost.style.transform = `translate(${mx - dragState.startX}px, ${my - dragState.startY}px) scale(1.05)`;
          
          const over = document.elementFromPoint(mx, my);
          const wsEl = over ? over.closest(".ws") : null;
          if (dragState.targetWsEl && dragState.targetWsEl !== wsEl) {
            dragState.targetWsEl.style.boxShadow = "";
          }
          if (wsEl) {
            wsEl.style.boxShadow = "inset 0 0 0 2px #34d399";
            dragState.targetWsEl = wsEl;
          }
        }
      };

      const onUp = (ue) => {
        if (!dragState) return;
        clearTimeout(dragState.timer);
        if (dragState.isDragging) {
          if (dragState.targetWsEl) {
            post("/hypr/move", { 
              workspace: Number(dragState.targetWsEl.dataset.ws),
              address: dragState.addr
            });
            dragState.targetWsEl.style.boxShadow = "";
          }
        } else {
          post("/hypr/focus", { address: dragState.addr });
        }
        dragState.ghost?.remove();
        cleanup();
      };

      const onCancel = () => {
        if (!dragState) return;
        clearTimeout(dragState.timer);
        dragState.ghost?.remove();
        if (dragState.targetWsEl) dragState.targetWsEl.style.boxShadow = "";
        cleanup();
      };

      const cleanup = () => {
        dragState = null;
        window.removeEventListener("mousemove", onMove);
        window.removeEventListener("mouseup", onUp);
        window.removeEventListener("touchmove", onMove);
        window.removeEventListener("touchend", onUp);
        window.removeEventListener("touchcancel", onCancel);
      };

      window.addEventListener("mousemove", onMove, { passive: false });
      window.addEventListener("mouseup", onUp);
      window.addEventListener("touchmove", onMove, { passive: false });
      window.addEventListener("touchend", onUp);
      window.addEventListener("touchcancel", onCancel);
    };

    b.addEventListener("mousedown", handleStart);
    b.addEventListener("touchstart", handleStart, { passive: false });
  });
}

function renderAudio(s) {
  const a = s.audio;
  if (!a) return;
  if (!volDragging && a.volume != null) {
    document.querySelectorAll(".vol-slider").forEach((sl) => (sl.value = a.volume));
    document.querySelectorAll(".vol-val").forEach((el) => (el.textContent = a.volume));
  }
  // mutes go red — toggles every element carrying data-mute (big buttons + rail/panel glyphs)
  document.querySelectorAll('[data-mute="mic"]').forEach((b) => b.classList.toggle("muted", !!a.mic_muted));
  document.querySelectorAll('[data-mute="sink"]').forEach((b) => b.classList.toggle("muted", !!a.sink_muted));
  const np = a.now_playing;
  const npEl = document.getElementById("now-playing");
  const newTitle = np && np.title ? `${np.status === "Playing" ? "▶" : "‖"} ${np.title}` : "—";
  if (npEl.textContent !== newTitle) {
    npEl.textContent = newTitle;
    // Restart ping-pong: measure overflow after paint, then re-arm animation.
    npEl.style.setProperty("--np-play", "paused");
    requestAnimationFrame(() => {
      const mask = npEl.parentElement;
      const overflow = npEl.scrollWidth - (mask ? mask.clientWidth : npEl.scrollWidth);
      if (overflow > 2) {
        // speed: ~60px/s feels natural; clamp between 4s and 14s
        const dur = Math.min(30, Math.max(12, overflow / 60 * 3 + 6));
        npEl.style.setProperty("--np-tx", `-${overflow}px`);
        npEl.style.setProperty("--np-dur", `${dur.toFixed(1)}s`);
        // Force animation restart by toggling animation-name
        npEl.style.animationName = "none";
        requestAnimationFrame(() => {
          npEl.style.animationName = "";
          npEl.style.setProperty("--np-play", "running");
        });
      } else {
        npEl.style.setProperty("--np-tx", "0px");
        npEl.style.setProperty("--np-play", "paused");
      }
    });
  }
  const art = document.getElementById("art");
  const npEmpty = document.getElementById("np-empty");
  const showEmpty = () => npEmpty && npEmpty.classList.remove("hidden");
  const hideEmpty = () => npEmpty && npEmpty.classList.add("hidden");
  if (np && np.art_key) {
    if (art.dataset.k !== np.art_key) {       // only refetch when the track changes
      art.dataset.k = np.art_key;
      art.onerror = () => { art.classList.add("hidden"); showEmpty(); };
      art.onload = () => { art.classList.remove("hidden"); hideEmpty(); };
      art.src = `/media/art?k=${np.art_key}`;
    }
  } else {
    art.classList.add("hidden");
    art.removeAttribute("src");
    art.dataset.k = "";
    showEmpty();
  }
  // transport play/pause glyph reflects state on every copy (rail + Audio panel)
  const ppGlyph = np && np.status === "Playing" ? "❚❚" : "▶";
  document.querySelectorAll(".pp-btn").forEach((b) => (b.textContent = ppGlyph));
  // seek bar
  if (seekSlider && !seekDragging) {
    const hasTrack = np && np.duration_us > 0;
    seekWrap && seekWrap.classList.toggle("no-track", !hasTrack);
    if (hasTrack) {
      seekSlider.dataset.durUs = np.duration_us;
      const ratio = Math.min(1, (np.position_us ?? 0) / np.duration_us);
      seekSlider.value = Math.round(ratio * 1000);
      if (seekElapsed) seekElapsed.textContent = fmtTime(np.position_us);
      if (seekTotal)   seekTotal.textContent   = fmtTime(np.duration_us);
    } else {
      seekSlider.dataset.durUs = 0;
      seekSlider.value = 0;
      if (seekElapsed) seekElapsed.textContent = "0:00";
      if (seekTotal)   seekTotal.textContent   = "0:00";
    }
  }
  document.getElementById("sink-list").innerHTML = (a.sinks || []).map((d) =>
    `<div class="sink dev w-full cursor-pointer text-sm ${d.active ? "on" : ""}" data-id="${d.id}"><span class="dot"></span><span class="flex-1 min-w-0 truncate">${d.name}</span></div>`).join("");
  document.querySelectorAll(".sink").forEach((b) =>
    (b.onclick = () => post("/audio/sink", { id: Number(b.dataset.id) })));
  document.getElementById("source-list").innerHTML = (a.sources || []).map((d) =>
    `<div class="source dev w-full cursor-pointer text-sm ${d.active ? "on" : ""}" data-id="${d.id}"><span class="dot"></span><span class="flex-1 min-w-0 truncate">${d.name}</span></div>`).join("");
  document.querySelectorAll(".source").forEach((b) =>
    (b.onclick = () => post("/audio/source", { id: Number(b.dataset.id) })));
}

function renderSystem(s) {
  const ts = s.sysinfo?.tailscale;
  if (ts) document.getElementById("ts-status").textContent =
    `Tailscale: ${ts.up ? "up" : "down"} · ${ts.self} · ${ts.online}/${ts.total} peers online`;
  const procs = s.sysinfo?.procs || [];
  document.getElementById("proc-list").innerHTML = procs.map((p) =>
    `<div class="proc">
      <span class="pn">${p.name}</span>
      <span class="pbar"><span class="pfill" style="width:${Math.min(100, Number(p.mem) || 0)}%"></span></span>
      <span class="pv">${p.mem}%</span>
      <button class="kill px-1" data-pid="${p.pid}" ${p.mine ? "" : "disabled"}>✕</button>
    </div>`).join("");
  document.querySelectorAll(".kill").forEach((b) =>
    (b.onclick = () => b.disabled || post("/sys/kill", { pid: Number(b.dataset.pid) })));
}

// ---- common context-aware controls (context.json apps + call), below the home columns ----
// Media transport moved to the rail; this strip carries the call quick-mute and the
// config-driven per-app buttons (keyed by focused/persist window class). Empty -> hidden.
let ctxConfig = { apps: {}, call_apps: [] };
async function loadContextConfig() {
  try { ctxConfig = await (await fetch("/context")).json(); } catch (_) {}
}
function ctxLoose(a, b) { return a === b || a.includes(b) || b.includes(a); }

function renderContext(s) {
  const strip = document.getElementById("ctx-home");
  if (!strip) return;
  const esc = (x) => String(x).replace(/</g, "&lt;");
  const wins = s.hypr?.windows || [];
  const focusedCls = (wins.find((w) => w.focused) || {}).cls || s.hypr?.active_window?.class || "";
  let html = "";

  // call — quick-mute + jump to the call window
  const calls = ctxConfig.call_apps || [];
  const rec = (s.audio?.recording || []).filter((r) => calls.some((a) => ctxLoose(r, a)));
  if (rec.length) {
    const muted = s.audio?.mic_muted;
    const callWin = wins.find((w) => w.cls && rec.some((r) => ctxLoose(w.cls, r)));
    html += `<div class="ctxrow"><span class="ctxlab">Call</span>`
      + `<button class="ctxbtn${muted ? " muted" : ""}" data-ctx="callmute">${muted ? "UNMUTE" : "MUTE"}</button>`
      + (callWin ? `<button class="ctxbtn" data-ctx="callfocus" data-addr="${callWin.address}">↪ call</button>` : "")
      + `</div>`;
  }

  // per-app controls — config-driven, keyed by window class (focused, or persist-present)
  for (const [cls, app] of Object.entries(ctxConfig.apps || {})) {
    const present = app.show === "persist" ? wins.some((w) => w.cls === cls) : focusedCls === cls;
    if (!present) continue;
    const btns = (app.controls || []).map((c) =>
      `<button class="ctxbtn" data-ctx="key" data-cls="${esc(cls)}" data-key="${esc(c.key)}" data-mods="${esc(c.mods || "")}">${esc(c.label || c.key)}</button>`).join("");
    html += `<div class="ctxrow"><span class="ctxlab">${esc(app.label || cls)}</span>${btns}</div>`;
  }

  strip.innerHTML = html;
  strip.querySelectorAll('[data-ctx="callmute"]').forEach((b) =>
    (b.onclick = () => post("/audio/mute", { target: "mic" })));
  strip.querySelectorAll('[data-ctx="callfocus"]').forEach((b) =>
    (b.onclick = () => post("/hypr/focus", { address: b.dataset.addr })));
  strip.querySelectorAll('[data-ctx="key"]').forEach((b) =>
    (b.onclick = () => post("/context/key", { cls: b.dataset.cls, key: b.dataset.key, mods: b.dataset.mods })));
}

function render(s) {
  lastState = s;
  renderStatus(s);
  renderContext(s);
  renderWorkspaces(s);
  renderAudio(s);
  renderSystem(s);
  cogFeed(s);
  // monitors may arrive after the Files tab was opened — keep its source list in sync
  if (activePanel === "files") {
    const sig = ((s.hypr && s.hypr.monitors) || []).map((m) => m.name).join(",");
    if (sig !== shotMonSig) populateShotMonitors();
  }
}

async function authOk() {
  try { return (await fetch("/input/available")).status !== 401; }
  catch (_) { return true; } // network blip: keep trying
}

function connect() {
  if (mainSock && mainSock.readyState <= 1) return;   // already open/connecting — no dupes
  const proto = location.protocol === "https:" ? "wss" : "ws";
  const sock = new WebSocket(`${proto}://${location.host}/ws`);
  mainSock = sock;
  const dot = document.getElementById("st-conn");
  sock.onopen = () => dot.classList.replace("bg-zinc-600", "bg-emerald-500");

  sock.onclose = async () => {
    dot.classList.replace("bg-emerald-500", "bg-zinc-600");
    if (!(await authOk())) { location.href = "/login"; return; }
    setTimeout(connect, 2000);
  };

  sock.onmessage = (e) => render(JSON.parse(e.data));
}
// responsive density: tag <html> so the UI can adapt to a fine-pointer / wide screen
// (laptop) vs the phone. Deliberately light — deeper restyling rides with the deferred
// visuals refresh; this just enables it without aggressive layout changes.
function applyDensity() {
  document.documentElement.dataset.pointer =
    window.matchMedia("(pointer: fine)").matches ? "fine" : "coarse";
  document.documentElement.dataset.wide = window.innerWidth >= 1024 ? "1" : "0";
}
applyDensity();
window.addEventListener("resize", applyDensity);

// ---- theme profile selector (System) — live hot-swap, no reload ----
(function initTheme() {
  const sel = document.getElementById("theme-profile");
  if (!sel) return;
  segInit(sel, async (profile) => {
    await post("/theme", { profile });
    const link = document.querySelector('link[href^="/theme.css"]');
    if (link) link.href = "/theme.css?t=" + Date.now();   // re-fetch the skin in place
  });
  // Ambient (cogitator) settings live in the same config file/endpoint.
  const idleSeg = document.getElementById("amb-idle"), lit = document.getElementById("amb-lit");
  segInit(idleSeg, async (v) => { cogCfg.idle_min = +v; await post("/theme", { ambient: { idle_min: +v } }); });
  if (lit) lit.onclick = async () => {
    cogCfg.liturgy = !cogCfg.liturgy;
    lit.classList.toggle("on", cogCfg.liturgy);
    await post("/theme", { ambient: { liturgy: cogCfg.liturgy } });
  };
  fetch("/theme").then((r) => r.json()).then((d) => {
    if (d.profile) segSet(sel, d.profile);
    if (d.ambient) {
      Object.assign(cogCfg, d.ambient);
      segSet(idleSeg, String(cogCfg.idle_min));
      if (lit) lit.classList.toggle("on", !!cogCfg.liturgy);
    }
  }).catch(() => {});
})();

// ---- Ambient Cogitator (V2.5) ----------------------------------------------
// Idle screen: instrument cluster (device-grouped dials, peak-hold ghosts),
// clock, duotone now-playing, liturgy. ONE locked screen — not a widget farm.
const cogCfg = { idle_min: 2, liturgy: true };
const cogEl = document.getElementById("cog");
const COG_RM = window.matchMedia && matchMedia("(prefers-reduced-motion: reduce)").matches;
let cogOn = false, cogLast = Date.now(), cogRaf = 0, cogTick = null, cogLit = null;
let cogDisp = null, cogPeak = null, cogLitIdx = 0, cogArtKey = "";

function cogCur() {
  const t = lastState.telemetry || {}, g = t.gpu || {}, c = t.cpu || {};
  return { g: g.util ?? 0, gt: g.temp ?? 0, vr: g.vram_used ?? 0, vt: g.vram_total || 12288,
           c: c.util ?? 0, ct: c.temp ?? 0, ram: c.mem_used ?? 0, rt: c.mem_total || 32 };
}
function cogFeed() {                       // live peak accumulation while ambient
  if (!cogOn || !cogPeak) return;
  const cur = cogCur();
  for (const k in cur) if (cur[k] > (cogPeak[k] ?? 0)) cogPeak[k] = cur[k];
}

// colors: device grouping — GPU pair = theme primary, CPU pair = counter-color
// (amber, unless the primary itself is amber-adjacent -> green). Danger = red.
function cogColors() {
  const hex = getComputedStyle(document.documentElement).getPropertyValue("--p-primary").trim() || "#33ffa0";
  const n = hex.replace("#", "");
  const r = parseInt(n.slice(0, 2), 16), g = parseInt(n.slice(2, 4), 16), b = parseInt(n.slice(4, 6), 16);
  const mx = Math.max(r, g, b), mn = Math.min(r, g, b);
  let h = 0;
  if (mx !== mn) {
    h = mx === r ? (g - b) / (mx - mn) : mx === g ? 2 + (b - r) / (mx - mn) : 4 + (r - g) / (mx - mn);
    h = (h * 60 + 360) % 360;
  }
  const amberish = Math.abs(h - 41) < 40;
  return { P: `${r},${g},${b}`, A: amberish ? "51,255,160" : "255,176,0", R: "255,82,82" };
}

function cogMetrics(cur, C) {
  return [
    { k: "g",   lab: "GPU",  unit: "%", lo: 0,  max: 100,    warn: .80, col: C.P, fmt: (v) => v.toFixed(0) },
    { k: "gt",  lab: "GPU°", unit: "°", lo: 30, max: 90,     warn: .75, col: C.P, fmt: (v) => v.toFixed(0) },
    { k: "c",   lab: "CPU",  unit: "%", lo: 0,  max: 100,    warn: .80, col: C.A, fmt: (v) => v.toFixed(0) },
    { k: "ct",  lab: "CPU°", unit: "°", lo: 30, max: 90,     warn: .75, col: C.A, fmt: (v) => v.toFixed(0) },
    { k: "vr",  lab: "VRAM", unit: "G", lo: 0,  max: cur.vt, warn: .85, col: C.P, fmt: (v) => (v / 1024).toFixed(1) },
    { k: "ram", lab: "RAM",  unit: "G", lo: 0,  max: cur.rt, warn: .85, col: C.A, fmt: (v) => v.toFixed(1) },
  ];
}

function cogDraw() {
  const cv = document.getElementById("cog-cv"), ctx = cv.getContext("2d");
  const dpr = devicePixelRatio || 1, box = cv.parentElement.getBoundingClientRect();
  if (cv.width !== Math.round(box.width * dpr)) { cv.width = Math.round(box.width * dpr); cv.height = Math.round(box.height * dpr); }
  const W = cv.width, H = cv.height, px = (n) => n * dpr;
  const cur = cogCur(), C = cogColors(), M = cogMetrics(cur, C);
  if (!cogDisp) cogDisp = { ...cur };
  for (const k in cur) cogDisp[k] = COG_RM ? cur[k] : cogDisp[k] + (cur[k] - cogDisp[k]) * .06;
  const frac = (m, v) => Math.max(0, Math.min(1, (v - m.lo) / (m.max - m.lo)));
  ctx.clearRect(0, 0, W, H);

  const dials = M.slice(0, 4), bars = M.slice(4);
  const zoneH = H * .66, r = Math.min(W / 9.2, zoneH / 2.55);
  const a0 = Math.PI * .75, span = Math.PI * 1.5, CX = [.125, .365, .635, .875];
  dials.forEach((m, i) => {
    const cx = W * CX[i], cy = zoneH * .52, col = m.col;
    const f = frac(m, cogDisp[m.k]), fp = frac(m, (cogPeak || cur)[m.k]), hot = f >= m.warn;
    ctx.save();
    ctx.lineWidth = px(2); ctx.strokeStyle = `rgba(${col},.20)`;
    ctx.beginPath(); ctx.arc(cx, cy, r, a0, a0 + span); ctx.stroke();
    ctx.strokeStyle = `rgba(${C.R},.4)`;
    ctx.beginPath(); ctx.arc(cx, cy, r, a0 + span * m.warn, a0 + span); ctx.stroke();
    for (let t = 0; t <= 10; t++) {
      const a = a0 + span * t / 10, big = t % 5 === 0;
      ctx.strokeStyle = `rgba(${col},${big ? .32 : .13})`; ctx.lineWidth = px(big ? 1.6 : 1);
      ctx.beginPath();
      ctx.moveTo(cx + Math.cos(a) * (r - px(big ? 7 : 4)), cy + Math.sin(a) * (r - px(big ? 7 : 4)));
      ctx.lineTo(cx + Math.cos(a) * r, cy + Math.sin(a) * r); ctx.stroke();
    }
    const ap = a0 + span * fp;                                  // peak-hold ghost
    ctx.strokeStyle = `rgba(${col},.22)`; ctx.lineWidth = px(1.6);
    ctx.beginPath(); ctx.moveTo(cx, cy);
    ctx.lineTo(cx + Math.cos(ap) * r * .82, cy + Math.sin(ap) * r * .82); ctx.stroke();
    const av = a0 + span * f;                                   // needle
    ctx.strokeStyle = hot ? `rgba(${C.R},.9)` : `rgba(${col},.85)`;
    ctx.shadowColor = ctx.strokeStyle; ctx.shadowBlur = px(8); ctx.lineWidth = px(2.2);
    ctx.beginPath(); ctx.moveTo(cx - Math.cos(av) * r * .12, cy - Math.sin(av) * r * .12);
    ctx.lineTo(cx + Math.cos(av) * r * .82, cy + Math.sin(av) * r * .82); ctx.stroke();
    ctx.shadowBlur = 0;
    ctx.fillStyle = `rgba(${col},.75)`;
    ctx.beginPath(); ctx.arc(cx, cy, px(2.5), 0, 7); ctx.fill();
    ctx.fillStyle = hot ? `rgba(${C.R},.9)` : `rgba(${col},.78)`;
    ctx.font = `600 ${px(16)}px "IBM Plex Mono",monospace`; ctx.textAlign = "center";
    ctx.fillText(m.fmt(cogDisp[m.k]) + m.unit, cx, cy + r * .62);
    ctx.fillStyle = `rgba(${col},.35)`; ctx.font = `${px(10)}px "IBM Plex Mono",monospace`;
    ctx.fillText(m.lab, cx, cy + r + px(15));
    ctx.restore();
  });
  bars.forEach((m, i) => {                                      // VRAM under GPU pair, RAM under CPU pair
    const bw = W * .42, bh = px(12), bx = W * (i === 0 ? .05 : .53), by = H * .78, col = m.col;
    const f = frac(m, cogDisp[m.k]), fp = frac(m, (cogPeak || cur)[m.k]), hot = f >= m.warn;
    ctx.save();
    ctx.strokeStyle = `rgba(${col},.22)`; ctx.lineWidth = px(1);
    ctx.strokeRect(bx, by, bw, bh);
    ctx.fillStyle = hot ? `rgba(${C.R},.75)` : `rgba(${col},.5)`;
    ctx.shadowColor = `rgba(${col},.45)`; ctx.shadowBlur = px(5);
    ctx.fillRect(bx + px(1.5), by + px(1.5), (bw - px(3)) * f, bh - px(3));
    ctx.shadowBlur = 0;
    ctx.strokeStyle = `rgba(${col},.4)`;
    ctx.beginPath(); ctx.moveTo(bx + bw * fp, by - px(2)); ctx.lineTo(bx + bw * fp, by + bh + px(2)); ctx.stroke();
    ctx.fillStyle = `rgba(${col},.55)`; ctx.font = `${px(10)}px "IBM Plex Mono",monospace`; ctx.textAlign = "left";
    const total = m.k === "vr" ? (cur.vt / 1024).toFixed(0) : cur.rt.toFixed(0);
    ctx.fillText(`${m.lab} ${m.fmt(cogDisp[m.k])}/${total}${m.unit}`, bx, by + bh + px(14));
    ctx.restore();
  });
}

// Decorative phosphor scope in the reserved #cog-socket-wave slot. Not a real level
// meter (the idle phone can't reach the rig's live audio cheaply) — it comes alive when
// the rig is Playing and eases to a calm idle ripple when it isn't. Honest eye-candy.
let cogWaveAmp = 0.06;
function cogWave(ts) {
  const cv = document.getElementById("cog-wave-cv");
  if (!cv) return;
  const ctx = cv.getContext("2d"), dpr = devicePixelRatio || 1, box = cv.getBoundingClientRect();
  if (cv.width !== Math.round(box.width * dpr)) { cv.width = Math.round(box.width * dpr); cv.height = Math.round(box.height * dpr); }
  const W = cv.width, H = cv.height, C = cogColors();
  ctx.clearRect(0, 0, W, H);
  const playing = lastState.audio?.now_playing?.status === "Playing";
  cogWaveAmp += ((playing ? 1 : 0.06) - cogWaveAmp) * 0.05;   // ease between live / idle
  const t = COG_RM ? 0.5 : ts / 1000, mid = H / 2, amp = H * 0.34 * cogWaveAmp;
  ctx.beginPath();
  for (let x = 0; x <= W; x += dpr) {
    const u = x / W;
    const y = mid + amp * (0.6 * Math.sin(u * 22 - t * 6) + 0.3 * Math.sin(u * 41 + t * 3.3) + 0.1 * Math.sin(u * 90 - t * 11));
    x === 0 ? ctx.moveTo(x, y) : ctx.lineTo(x, y);
  }
  ctx.lineWidth = dpr; ctx.strokeStyle = `rgb(${C.P})`;
  ctx.shadowColor = `rgb(${C.P})`; ctx.shadowBlur = 4 * dpr;
  ctx.globalAlpha = 0.3 + 0.6 * cogWaveAmp;
  ctx.stroke();
  ctx.shadowBlur = 0; ctx.globalAlpha = 1;
}

function cogChrome() {                     // clock/date/uptime/now-playing refresh
  const d = new Date(), hh = String(d.getHours()).padStart(2, "0"), mm = String(d.getMinutes()).padStart(2, "0");
  document.getElementById("cog-clock").innerHTML =
    `${hh}<span class="cln${d.getSeconds() % 2 ? " off" : ""}">:</span>${mm}`;
  document.getElementById("cog-date").textContent =
    d.toLocaleDateString("en-GB", { weekday: "short", day: "2-digit", month: "short" }).toUpperCase();
  const boot = lastState.sysinfo?.boot;
  if (boot) {
    const up = Math.max(0, Date.now() / 1000 - boot);
    document.getElementById("cog-uptime").textContent =
      `UPTIME ${Math.floor(up / 86400)}D ${String(Math.floor(up / 3600) % 24).padStart(2, "0")}H ${String(Math.floor(up / 60) % 60).padStart(2, "0")}M`;
  }
  const np = lastState.audio?.now_playing, wrap = document.getElementById("cog-artwrap");
  if (np && np.title) {
    document.getElementById("cog-np-status").textContent = np.status === "Playing" ? "▶ PLAYING" : "‖ PAUSED";
    document.getElementById("cog-np-title").textContent = np.title;
    if (np.art_key && np.art_key !== cogArtKey) {
      cogArtKey = np.art_key;
      document.getElementById("cog-art").src = `/media/art?k=${np.art_key}`;
    }
    wrap.classList.toggle("hidden", !np.art_key);
  } else {
    document.getElementById("cog-np-status").textContent = "— NO SIGNAL —";
    document.getElementById("cog-np-title").textContent = "";
    wrap.classList.add("hidden");
  }
}

const COG_LITANY = [
  () => "++ cogitator vigil unbroken ++",
  () => `++ thermal rites nominal : ${cogCur().gt.toFixed(0)}°C ++`,
  () => `++ vram offering : ${(cogCur().vr / 1024).toFixed(1)}G consecrated ++`,
  () => "++ the omnissiah preserves : all systems sanctified ++",
  () => `++ canticle of load : ${cogCur().g.toFixed(0)}% devotion ++`,
];

function cogShow() {
  if (cogOn) return;
  cogOn = true;
  cogDisp = null;
  cogPeak = { ...cogCur() };
  // Seed peaks from history since last touch — "was it pegged while I was away"
  // works even if the deck app was hidden/restarted during the away period.
  fetch("/history").then((r) => r.json()).then((h) => {
    if (!cogOn) return;
    for (const s of h) {
      if (s.t < cogLast) continue;
      for (const k of ["g", "gt", "vr", "c", "ct", "ram"])
        if (s[k] != null && s[k] > cogPeak[k]) cogPeak[k] = s[k];
    }
  }).catch(() => {});
  renderOutposts(); fetchFleet();   // instant from cached roster, then refresh live status
  document.getElementById("cog-liturgy").style.display = cogCfg.liturgy ? "" : "none";
  cogChrome(); cogEl.classList.remove("hidden");
  cogTick = setInterval(cogChrome, 500);
  cogLit = setInterval(() => {
    const el = document.getElementById("cog-liturgy");
    el.classList.add("fade");
    setTimeout(() => { cogLitIdx = (cogLitIdx + 1) % COG_LITANY.length; el.textContent = COG_LITANY[cogLitIdx](); el.classList.remove("fade"); }, 1200);
  }, 30000);
  let last = 0;
  const loop = (ts) => {
    if (!cogOn) return;
    if (ts - last >= 33) {
      last = ts;
      cogDraw();
      cogWave(ts);
      if (!COG_RM) {
        const t = ts / 1000;   // Lissajous pixel drift (AMOLED)
        document.getElementById("cog-drift").style.transform =
          `translate(${(8 * Math.sin(t / 29)).toFixed(2)}px, ${(8 * Math.sin(t / 37)).toFixed(2)}px)`;
      }
    }
    cogRaf = requestAnimationFrame(loop);
  };
  cogRaf = requestAnimationFrame(loop);
}

function cogHide() {
  if (!cogOn) return;
  cogOn = false;
  cancelAnimationFrame(cogRaf);
  clearInterval(cogTick); clearInterval(cogLit);
  cogEl.classList.add("hidden");           // exact tab untouched underneath
}

// entry/exit wiring: any activity resets idle; touch while ambient exits (and
// is swallowed — an exit tap must never click through to the deck).
// The overlay hides on pointerdown, so the gesture's synthesized `click` would
// otherwise land on whatever control is now underneath — eat that one click.
function cogEatClick() {
  const eat = (ce) => { ce.preventDefault(); ce.stopPropagation(); };
  document.addEventListener("click", eat, { capture: true, once: true });
  setTimeout(() => document.removeEventListener("click", eat, { capture: true }), 800);
}
["pointerdown", "keydown", "wheel"].forEach((ev) =>
  document.addEventListener(ev, (e) => {
    cogLast = Date.now();
    if (cogOn) {
      e.preventDefault(); e.stopPropagation(); cogHide();
      if (ev === "pointerdown") cogEatClick();
    }
  }, { capture: true, passive: false }));
setInterval(() => {
  if (!cogOn && document.visibilityState === "visible" && !window._voiceBusy
      && Date.now() - cogLast > cogCfg.idle_min * 60000) cogShow();
}, 5000);
document.getElementById("st-clock").addEventListener("click", cogShow);
document.addEventListener("visibilitychange", () => { if (document.visibilityState === "visible") cogLast = Date.now(); });

// ---- fleet host switcher (the wordmark) ----
let fleetHosts = [];
const _hostKey = (u) => { try { return new URL(u).hostname.split(".")[0]; } catch (_) { return ""; } };
const _selfKey = () => location.hostname.split(".")[0] || "deck";  // "you are here" from our own origin
async function fetchFleet() {
  try { const r = await fetch("/fleet"); fleetHosts = r.ok ? await r.json() : []; }
  catch (_) { fleetHosts = []; }
  renderHost();
  renderOutposts();   // the Cogitator's ambient fleet board reads the same roster
}
function renderHost() {
  const self = _selfKey();
  const here = fleetHosts.find((h) => _hostKey(h.url) === self);
  document.getElementById("host-name").textContent = (here ? here.name : self).toUpperCase();
  const menu = document.getElementById("host-menu");
  if (!fleetHosts.length) {
    menu.innerHTML = '<div class="px-3 py-2 text-xs text-zinc-500 whitespace-nowrap">no fleet.json</div>';
    return;
  }
  menu.innerHTML = fleetHosts.map((h) => {
    const isHere = _hostKey(h.url) === self;
    // green=online, grey=known-offline, amber=unknown (tailscale down / not in view)
    const dot = h.online === true ? "bg-emerald-500" : (h.online === false ? "bg-zinc-600" : "bg-amber-500");
    return `<button class="host-row w-full text-left flex items-center gap-2 px-3 py-2 text-sm
        ${isHere ? "text-emerald-400" : "text-zinc-200 hover:bg-zinc-800"}"
        data-url="${h.url}" data-here="${isHere ? 1 : 0}">
        <span class="w-2 h-2 rounded-full shrink-0 ${dot}"></span>
        <span class="flex-1 truncate">${(h.label || h.name).replace(/</g, "&lt;")}</span>
        ${isHere ? '<span class="text-[9px] tracking-widest text-emerald-500/70">HERE</span>' : ""}
      </button>`;
  }).join("");
  menu.querySelectorAll(".host-row").forEach((b) => (b.onclick = () => {
    if (b.dataset.here === "1") { hostMenu(false); return; }   // already on this bird
    location.href = b.dataset.url;                              // navigate: its own origin/cookie/WS
  }));
}
// Ambient fleet board in the Cogitator's reserved outposts socket. Display-only
// (the cog dismisses on any touch); dots semantic (green/grey/amber), names phosphor.
function renderOutposts() {
  const el = document.getElementById("cog-socket-outposts");
  if (!el) return;
  if (!fleetHosts.length) { el.textContent = "· V3 · OUTPOSTS ·"; return; }
  const self = _selfKey();
  const dot = (o) => o === true ? "#4dffa4" : (o === false ? "#4a5560" : "#ffb000");
  el.innerHTML =
    '<div style="opacity:.5;margin-bottom:7px;font-size:9px">· OUTPOSTS ·</div>' +
    '<div style="display:flex;flex-direction:column;gap:5px;font-size:12px;letter-spacing:.1em;text-align:left">' +
    fleetHosts.map((h) => {
      const here = _hostKey(h.url) === self;
      // whole-line brightness tracks reachability so "who's up" reads at a glance —
      // the dim 7px dot got missed on-device. here=full, online=bright, offline=dim.
      const op = here ? 1 : (h.online === true ? 0.9 : 0.32);
      return `<div style="display:flex;align-items:center;gap:8px;color:var(--p-primary);opacity:${op}">`
        + `<span style="color:${dot(h.online)};font-size:11px;line-height:1">●</span>`
        + `<span style="flex:1;white-space:nowrap;overflow:hidden;text-overflow:ellipsis">${(h.name || "").toUpperCase().replace(/</g, "&lt;")}</span>`
        + (here ? '<span style="opacity:.75;font-size:8px;letter-spacing:.2em">HERE</span>' : "")
        + "</div>";
    }).join("") +
    "</div>";
}
function hostMenu(show) {
  const menu = document.getElementById("host-menu");
  const willShow = show === undefined ? menu.classList.contains("hidden") : show;
  if (willShow) {
    // Position via the button's viewport rect. NOTE: set position:fixed INLINE — the
    // menu's bg-zinc-900+rounded-lg classes match theme.css's card rule
    // `.bg-zinc-900.rounded-lg{position:relative}` (specificity 0,2,0), which beats
    // Tailwind's `.fixed` (0,1,0) and would otherwise flow the menu off-screen. Inline
    // style outranks any selector, so this wins.
    const r = document.getElementById("host-switch").getBoundingClientRect();
    menu.style.position = "fixed";
    menu.style.left = Math.round(r.left) + "px";
    menu.style.top = Math.round(r.bottom + 4) + "px";
    fetchFleet();   // refresh dots on each open
  }
  menu.classList.toggle("hidden", !willShow);
}
document.getElementById("host-switch").addEventListener("click", (e) => { e.stopPropagation(); hostMenu(); });
document.addEventListener("click", (e) => { if (!e.target.closest("#host-switch,#host-menu")) hostMenu(false); });
fetchFleet();   // populate the wordmark on load

connect();
connectInput();
fetchBrightness();
loadContextConfig();

const btnFullscreen = document.getElementById("btn-fullscreen");
if (btnFullscreen) {
  const panelRemote = document.getElementById("panel-remote");
  btnFullscreen.addEventListener("click", () => {
    document.body.classList.toggle("remote-maximized");
    if (document.body.classList.contains("remote-maximized")) {
      btnFullscreen.textContent = "Minimize";
    } else {
      btnFullscreen.textContent = "Fullscreen";
    }
  });
}

setInterval(() => {
  document.getElementById("st-clock").textContent =
    new Date().toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
}, 1000);
