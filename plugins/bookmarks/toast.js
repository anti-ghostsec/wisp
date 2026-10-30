// Small notice shown inside the page (shadow DOM keeps page CSS out).
globalThis.wispToast = (text) => {
  const host = document.createElement("div");
  const root = host.attachShadow({ mode: "closed" });
  root.innerHTML = `<style>
    div { position: fixed; top: 14px; right: 14px; z-index: 2147483647; padding: 8px 14px;
          font: 13px system-ui, sans-serif; color: #fff; background: #222e; border-radius: 8px;
          box-shadow: 0 2px 10px #0004; transition: opacity .4s; }
  </style><div></div>`;
  root.querySelector("div").textContent = text;
  document.documentElement.appendChild(host);
  setTimeout(() => (root.querySelector("div").style.opacity = "0"), 1800);
  setTimeout(() => host.remove(), 2300);
};
