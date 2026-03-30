const CACHE_NAME = "pcplayerpicker-shell-v3";
const APP_SHELL = [
  "/",
  "/manifest.json",
  "/icon.svg",
  "/maskable-icon.svg",
  "/icon-192.png",
  "/icon-512.png",
];

function sanitizeHtmlForCache(html) {
  return html
    .replace(
      /<script\b[^>]*\bsrc=["']https:\/\/static\.cloudflareinsights\.com\/[^"']+["'][^>]*><\/script>/gi,
      "",
    )
    .replace(
      /<script\b[^>]*\bsrc=["']https:\/\/www\.googletagmanager\.com\/[^"']+["'][^>]*><\/script>/gi,
      "",
    );
}

async function cacheSanitizedIndex(cache, response) {
  if (!response || !response.ok) return;
  try {
    const html = await response.text();
    const sanitized = sanitizeHtmlForCache(html);
    await cache.put(
      "/index.html",
      new Response(sanitized, {
        headers: { "Content-Type": "text/html; charset=utf-8" },
      }),
    );
  } catch (_) {
    // Best effort: keep runtime working even if shell sanitization fails.
  }
}

async function precacheBuildAssets(cache) {
  try {
    const response = await fetch("/index.html", { cache: "no-store" });
    if (!response.ok) return;
    const html = sanitizeHtmlForCache(await response.text());
    await cache.put(
      "/index.html",
      new Response(html, {
        headers: { "Content-Type": "text/html; charset=utf-8" },
      }),
    );
    const regex = /(?:src|href)=["']([^"']+\.(?:wasm|js|css))["']/gi;
    const assets = new Set();

    let match = null;
    while ((match = regex.exec(html)) !== null) {
      const url = new URL(match[1], self.location.origin);
      if (url.origin === self.location.origin && !url.pathname.startsWith("/api/")) {
        assets.add(url.pathname);
      }
    }
    await Promise.all(
      Array.from(assets).map((path) => cache.add(path).catch(() => undefined)),
    );
  } catch (_) {
    // Best effort: install should still succeed with app shell only.
  }
}

self.addEventListener("install", (event) => {
  event.waitUntil(
    (async () => {
      const cache = await caches.open(CACHE_NAME);
      await cache.addAll(APP_SHELL);
      await precacheBuildAssets(cache);
      await self.skipWaiting();
    })(),
  );
});

self.addEventListener("activate", (event) => {
  event.waitUntil(
    caches
      .keys()
      .then((keys) =>
        Promise.all(keys.filter((key) => key !== CACHE_NAME).map((key) => caches.delete(key))),
      )
      .then(() => self.clients.claim()),
  );
});

self.addEventListener("fetch", (event) => {
  const { request } = event;
  if (request.method !== "GET") {
    return;
  }

  const url = new URL(request.url);
  if (url.pathname.startsWith("/api/")) {
    return;
  }

  if (request.mode === "navigate") {
    event.respondWith(
      fetch(request)
        .then((response) => {
          caches.open(CACHE_NAME).then((cache) => cacheSanitizedIndex(cache, response.clone()));
          return response;
        })
        .catch(async () => {
          const cached = await caches.match("/index.html");
          return cached || caches.match("/");
        }),
    );
    return;
  }

  event.respondWith(
    caches.match(request).then((cached) => {
      const network = fetch(request)
        .then((response) => {
          if (response.ok || response.type === "opaque") {
            const copy = response.clone();
            caches.open(CACHE_NAME).then((cache) => cache.put(request, copy));
          }
          return response;
        })
        .catch(() => cached);

      return cached || network;
    }),
  );
});
