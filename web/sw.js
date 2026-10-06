// hyprhand service worker — v1
// Caches the app shell for instant load and offline resilience.
// Also bridges the Android share-target: the OS POST /share arrives without
// the SameSite=Strict cookie, so we intercept and re-POST to /share/api
// (same-origin, cookie attaches) then return a confirmation page.
const CACHE = "hyprhand-v2";
const SHELL = ["/static/app.js", "/static/manifest.webmanifest"];

self.addEventListener("install", (e) => {
  e.waitUntil(
    caches.open(CACHE)
      .then((c) => Promise.allSettled(SHELL.map((u) => c.add(u))))
      .then(() => self.skipWaiting())
  );
});

self.addEventListener("activate", (e) => {
  e.waitUntil(
    caches.keys().then((keys) =>
      Promise.all(keys.filter((k) => k !== CACHE).map((k) => caches.delete(k)))
    ).then(() => self.clients.claim())
  );
});

self.addEventListener("fetch", (e) => {
  const url = new URL(e.request.url);
  if (url.pathname === "/share" && e.request.method === "POST") {
    e.respondWith(handleShare(e.request));
    return;
  }
  if (url.pathname.startsWith("/ws") || e.request.method !== "GET") return;
  if (e.request.mode === "navigate") return;
  // Only cache static assets; never API responses (clipboard, screenshots, config...).
  if (!url.pathname.startsWith("/static/")) return;
  e.respondWith(
    fetch(e.request).then((r) => {
      if (r.ok) {
        const copy = r.clone();
        caches.open(CACHE).then((c) => c.put(e.request, copy)).catch(() => {});
      }
      return r;
    }).catch(async () => (await caches.match(e.request)) || Response.error())
  );
});

async function handleShare(req) {
  let r;
  try {
    const fd = await req.formData();
    r = await fetch("/share/api", { method: "POST", body: fd, credentials: "same-origin" });
  } catch (err) {
    return sharePage(false, "RIG UNREACHABLE", "check tailscale / hyprhand service");
  }
  if (r.status === 401 || r.status === 403 || r.redirected)
    return sharePage(false, "NOT LOGGED IN", "open hyprhand once, then share again");
  let j = null;
  try { j = await r.json(); } catch (e) {}
  if (!j || !j.ok) return sharePage(false, "SHARE FAILED", (j && j.detail) || `status ${r.status}`);
  return sharePage(true, "SENT → RIG", j.detail || "");
}

function sharePage(ok, head, detail) {
  const esc = (s) => String(s).replace(/&/g, "&amp;").replace(/</g, "&lt;");
  const c = ok ? "#33ffa0" : "#ff5252";
  return new Response(
`<!doctype html><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>hyprhand share</title>
<body style="margin:0;height:100vh;display:flex;flex-direction:column;align-items:center;justify-content:center;gap:12px;background:#000;color:${c};font-family:monospace;text-align:center">
<div style="font-size:22px;letter-spacing:.15em">${esc(head)} ${ok ? "✓" : "✗"}</div>
<div style="opacity:.65;font-size:14px;padding:0 24px">${esc(detail)}</div>
<div style="opacity:.35;font-size:12px;margin-top:18px">tap back to return</div>
</body>`,
    { headers: { "Content-Type": "text/html; charset=utf-8" } });
}
