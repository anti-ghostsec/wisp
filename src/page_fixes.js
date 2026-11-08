// Built into Wisp, in every page and frame (in a world of its own, so pages
// can't see or undo it).
//
// Sliders a page marks draggable (YouTube's volume) start a real drag and
// drop when they're slid in WebKit: the drag takes the pointer, the page
// stops hearing it move, and the slider stays where it was (a click still
// works). A drag that starts on a slider is the slider being moved, never
// something to drop somewhere else, so that drag is called off.
addEventListener("dragstart", (e) => {
  const el = e.target instanceof Element ? e.target : e.target && e.target.parentElement;
  if (el && el.closest('[role="slider"], input[type="range"]')) e.preventDefault();
}, true);

// A big copy (a whole page) would hang Wisp: GTK hangs handing over
// anything past 256 KB on X11, and the formatted copy is several times the
// text. So past 50 000 characters the plain text goes to Wisp's own
// clipboard sender instead (bigcopy.rs). Only where Wisp offers it (X11).
addEventListener("copy", (e) => {
  const send = globalThis.webkit && webkit.messageHandlers && webkit.messageHandlers.wispcopy;
  if (!send) return;
  const a = document.activeElement;
  const text = a && (a.tagName === "TEXTAREA" || a.tagName === "INPUT") && typeof a.selectionStart === "number"
    ? a.value.slice(a.selectionStart, a.selectionEnd)
    : String(getSelection());
  if (text.length < 50000) return;
  e.preventDefault();
  e.stopImmediatePropagation();
  send.postMessage(text);
}, true);

// YouTube's player has a menu of its own, shown on every other right-click
// with Wisp's in between, and left open under Wisp's: which menu came
// depended on how many clicks there had been, and two could show at once.
// So on the player a right-click always gives Wisp's menu (Pause, Mute,
// Loop, Fullscreen, Download video), the same every time. Shift+right-click
// leaves the click to YouTube, for its menu (Copy video URL, Stats...).
// This listens on the window before the page's own listeners, so YouTube
// never hears of a plain right-click.
const closeYouTubeMenu = () => {
  for (const m of document.querySelectorAll(".ytp-contextmenu")) m.style.display = "none";
};
addEventListener("contextmenu", (e) => {
  const t = e.target;
  const player = t instanceof Element ? t.closest(".html5-video-player") : null;
  if (!player || e.shiftKey) return;
  e.stopImmediatePropagation();
  closeYouTubeMenu();
}, true);
// A YouTube menu still open closes when Wisp's comes.
addEventListener("contextmenu", (e) => {
  setTimeout(() => {
    if (!e.defaultPrevented) closeYouTubeMenu();
  }, 0);
});
