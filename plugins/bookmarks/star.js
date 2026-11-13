// Toggle a bookmark for the current page when the toolbar star (Ctrl+D) is used.
// A web page is only shown its own site's bookmarks (Wisp sees to that), and
// may only add, remove or move those.
wisp.on("action", async ({ id }) => {
  if (id !== "toggle") return;
  try {
    const url = location.href;
    const all = await wisp.sync.list("b_");
    const existing = all.find((b) => b.url === url);
    if (existing) {
      await wisp.sync.remove(existing.id);
      wispToast("Bookmark removed");
    } else {
      const title = (document.title || location.hostname).trim();
      const id = await wisp.sync.add({ title: "b_" + title, url });
      // New bookmarks go first on the bar, where you'll see them.
      await wisp.sync.move(id);
      wispToast("Bookmarked");
    }
  } catch (e) {
    wispToast("Bookmark failed: " + e.message);
  }
});
