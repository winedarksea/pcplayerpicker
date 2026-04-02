const CACHE_NAME = "pcplayerpicker-shell-v5";
const APP_SHELL = [
  "/",
  "/index.html",
  "/coach",
  "/coach/setup",
  "/tutorial",
  "/faq",
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

async function cachedNavigationResponse() {
  const cachedIndex = await caches.match("/index.html");
  if (cachedIndex) {
    return cachedIndex;
  }
  const cachedRoot = await caches.match("/");
  if (cachedRoot) {
    return cachedRoot;
  }
  return new Response("Offline", {
    status: 503,
    headers: { "Content-Type": "text/plain; charset=utf-8" },
  });
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
      await Promise.all(APP_SHELL.map((path) => cache.add(path).catch(() => undefined)));
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
  
  // Do not cache cross-origin requests (e.g., Cloudflare Insights, external APIs)
  if (url.hostname !== self.location.hostname) {
    return;
  }

  if (request.mode === "navigate") {
    event.respondWith(
      (async () => {
        const cachedIndex = await caches.match("/index.html");
        const cachedRoot = await caches.match("/");
        const cached = cachedIndex || cachedRoot;

        // Keep navigation launch fully offline-first once shell exists; refresh cache in background.
        const refresh = fetch(request)
          .then((response) => {
            if (!response.ok) {
              return cachedNavigationResponse();
            }
            caches.open(CACHE_NAME).then((cache) => cacheSanitizedIndex(cache, response.clone()));
            return response;
          })
          .catch(() => cachedNavigationResponse());

        if (cached) {
          refresh.catch(() => undefined);
          return cached;
        }
        return refresh;
      })(),
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
