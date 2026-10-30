// Runs in the page's own world (before page scripts) so overrides are visible to it.
(() => {
  const define = (obj, prop, value) => {
    try {
      Object.defineProperty(obj, prop, { get: () => value, configurable: true });
    } catch (_) {}
  };
  const nav = Navigator.prototype;
  define(nav, "hardwareConcurrency", 4);
  define(nav, "deviceMemory", 8);
  define(nav, "maxTouchPoints", 0);
  define(nav, "doNotTrack", "1");
  define(nav, "globalPrivacyControl", true);
  if ("getBattery" in nav) define(nav, "getBattery", undefined);
  if ("connection" in nav) define(nav, "connection", undefined);

  // Report the most common color depth.
  const scr = Screen.prototype;
  define(scr, "colorDepth", 24);
  define(scr, "pixelDepth", 24);

  // Add imperceptible per-session noise to canvas readbacks.
  const seed = (Math.random() * 255) | 0;
  const noisify = (canvas) => {
    try {
      const ctx = canvas.getContext("2d");
      if (!ctx || !canvas.width || !canvas.height) return;
      const img = ctx.getImageData(0, 0, Math.min(canvas.width, 16), 1);
      for (let i = 0; i < img.data.length; i += 4) img.data[i] ^= (seed >> (i % 8)) & 1;
      ctx.putImageData(img, 0, 0);
    } catch (_) {}
  };
  const toDataURL = HTMLCanvasElement.prototype.toDataURL;
  HTMLCanvasElement.prototype.toDataURL = function (...args) {
    noisify(this);
    return toDataURL.apply(this, args);
  };
  const toBlob = HTMLCanvasElement.prototype.toBlob;
  HTMLCanvasElement.prototype.toBlob = function (...args) {
    noisify(this);
    return toBlob.apply(this, args);
  };
})();
