// Toggle a bookmark for the current page when the toolbar star (Ctrl+D) is used.
wisp.on("action", async ({ id }) => {
  if (id !== "toggle") return;
  try {
    const url = location.href;
    const existing = (await wisp.sync.list("b_")).find((b) => b.url === url);
    if (existing) {
      await wisp.sync.remove(existing.id);
      wispToast("Bookmark removed");
    } else {
      const title = (document.title || location.hostname).trim();
      await wisp.sync.add({ title: "b_" + title, url });
      wispToast("Bookmarked");
    }
  } catch (e) {
    wispToast("Bookmark failed: " + e.message);
  }
});
