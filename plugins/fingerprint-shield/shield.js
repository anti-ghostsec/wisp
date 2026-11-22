// Runs in the page's own world (before page scripts) so overrides are visible to it.
// It covers a few device values, canvas readbacks and WebGL; not fonts,
// audio, the time zone, screen size or the user agent. A careful site can
// still tell it's there.
(() => {
  // Gecko (Wisp for Android) has protection of its own, always on, and on
  // Tor the fingerprinting resistance Tor Browser uses: values every Tor
  // user shares. Changing them here (touch points, device memory, canvas
  // noise) would only mark Wisp's users, so the shield stays out of it.
  if (typeof window.mozInnerScreenX === "number" || CSS.supports("-moz-user-focus", "normal")) return;
  // What we replace still reads as native code to the page.
  const natives = new WeakMap();
  const fnToString = Function.prototype.toString;
  const toStringShim = function () {
    return fnToString.call(natives.get(this) || this);
  };
  natives.set(toStringShim, fnToString);
  Function.prototype.toString = toStringShim;
  const wrap = (obj, name, make) => {
    const orig = obj && obj[name];
    if (typeof orig !== "function") return;
    const fn = make(orig);
    natives.set(fn, orig);
    obj[name] = fn;
  };
  const define = (obj, prop, value) => {
    try {
      const old = Object.getOwnPropertyDescriptor(obj, prop);
      const get = () => value;
      if (old && old.get) natives.set(get, old.get);
      Object.defineProperty(obj, prop, { get, configurable: true, enumerable: old ? old.enumerable : true });
    } catch (_) {}
  };

  const nav = Navigator.prototype;
  // Only what this engine already has: an API WebKit lacks (such as
  // Chrome's deviceMemory) would mark the page's visitor as a Wisp user.
  if ("hardwareConcurrency" in nav) define(nav, "hardwareConcurrency", 4);
  if ("deviceMemory" in nav) define(nav, "deviceMemory", 8);
  if ("maxTouchPoints" in nav) define(nav, "maxTouchPoints", 0);
  // No Do Not Track or Global Privacy Control here: saying so without
  // the headers that go with them would only make Wisp stand out.
  if ("getBattery" in nav) define(nav, "getBattery", undefined);
  if ("connection" in nav) define(nav, "connection", undefined);

  // Report the most common color depth.
  const scr = Screen.prototype;
  define(scr, "colorDepth", 24);
  define(scr, "pixelDepth", 24);

  // Canvas and WebGL readbacks get faint noise that stays the same for this
  // page load: the same drawing reads back the same each time, so reading
  // twice doesn't reveal or cancel it, but it differs from one load to the
  // next. The noise goes on the copy that's handed out, never on the
  // canvas itself.
  const seed = crypto.getRandomValues(new Uint32Array(1))[0];
  const noiseAt = (i) => {
    let h = Math.imul(i ^ seed, 0x9e3779b1);
    h ^= h >>> 15;
    h = Math.imul(h, 0x85ebca77);
    return (h ^ (h >>> 13)) >>> 0;
  };
  // Flips the low bit of one color value in about one pixel in eight,
  // chosen by the pixel's place on the canvas.
  const noisePixels = (data, width, x0, y0) => {
    for (let i = 0; i < data.length; i += 4) {
      const p = i >> 2;
      const n = noiseAt((y0 + Math.floor(p / width)) * 65536 + x0 + (p % width));
      if ((n & 7) === 0 && data[i + 3] !== 0) data[i + ((n >>> 3) % 3)] ^= 1;
    }
  };

  const ctx2d = CanvasRenderingContext2D.prototype;
  const getImageData = ctx2d.getImageData;
  wrap(ctx2d, "getImageData", (orig) =>
    function (sx, sy, sw, sh, ...rest) {
      const img = orig.call(this, sx, sy, sw, sh, ...rest);
      try {
        noisePixels(img.data, img.width, sx | 0, sy | 0);
      } catch (_) {}
      return img;
    });

  // A noised copy of a canvas, for toDataURL and toBlob; null if it can't
  // be made (the canvas is then read as it is).
  const noisedCopy = (canvas) => {
    try {
      if (!canvas.width || !canvas.height) return null;
      const copy = document.createElement("canvas");
      copy.width = canvas.width;
      copy.height = canvas.height;
      const c = copy.getContext("2d");
      c.drawImage(canvas, 0, 0);
      const img = getImageData.call(c, 0, 0, copy.width, copy.height);
      noisePixels(img.data, img.width, 0, 0);
      c.putImageData(img, 0, 0);
      return copy;
    } catch (_) {
      return null;
    }
  };
  const canvasProto = HTMLCanvasElement.prototype;
  wrap(canvasProto, "toDataURL", (orig) =>
    function (...args) {
      return orig.apply(noisedCopy(this) || this, args);
    });
  wrap(canvasProto, "toBlob", (orig) =>
    function (...args) {
      return orig.apply(noisedCopy(this) || this, args);
    });

  // WebGL: noise on what readPixels hands back, and the engine's generic
  // vendor and renderer names in place of the graphics card's.
  const glProtos = [self.WebGLRenderingContext, self.WebGL2RenderingContext].filter(Boolean).map((c) => c.prototype);
  for (const gl of glProtos) {
    wrap(gl, "readPixels", (orig) =>
      function (x, y, w, h, format, type, pixels, ...rest) {
        const out = orig.call(this, x, y, w, h, format, type, pixels, ...rest);
        try {
          if (pixels instanceof Uint8Array && format === this.RGBA) noisePixels(pixels, w, x, y);
        } catch (_) {}
        return out;
      });
    wrap(gl, "getParameter", (orig) =>
      function (p) {
        // UNMASKED_VENDOR_WEBGL and UNMASKED_RENDERER_WEBGL.
        if (p === 0x9245) return orig.call(this, this.VENDOR);
        if (p === 0x9246) return orig.call(this, this.RENDERER);
        return orig.call(this, p);
      });
  }
})();
