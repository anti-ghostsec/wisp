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
