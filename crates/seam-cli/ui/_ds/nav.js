// seam — the section navigation.
//
// Three sections, not nine. The app had grown a tab per feature, which is how a tool for
// one job ends up looking like an admin console: a person opening seam wants to see their
// machines, occasionally add one, and rarely change something. Everything else — doctor,
// first run, update, roadmap — is consulted when something is wrong, so it belongs behind
// one door rather than four across the top.
(function () {
  "use strict";

  var SECTIONS = [
    ["/", "Machines"],
    ["/join.html", "Add a machine"],
    ["/settings.html", "Settings"]
  ];

  // Reached from Settings and from the health card, not from the top bar.
  var TUCKED = ["/doctor.html", "/pairing.html", "/transfers.html", "/update.html", "/onboarding.html", "/ideas.html"];

  var CSS = "nav.tabs{display:flex;gap:2px;margin-bottom:22px;border-bottom:1px solid var(--border-subtle);overflow-x:auto}" +
    "nav.tabs a{padding:10px 14px;font:500 var(--text-sm)/1.2 var(--font-sans);color:var(--text-secondary);text-decoration:none;border-bottom:2px solid transparent;white-space:nowrap}" +
    "nav.tabs a:hover{color:var(--text-primary);background:var(--surface-hover)}" +
    "nav.tabs a[aria-current=page]{color:var(--text-link);border-bottom-color:var(--border-accent)}" +
    "nav.tabs .back{margin-right:6px;color:var(--text-tertiary)}";

  function here() { return location.pathname === "" ? "/" : location.pathname; }

  function render() {
    var shell = document.querySelector(".shell");
    if (!shell) return;

    var existing = shell.querySelector("nav.tabs");
    if (existing) existing.remove();

    var style = document.createElement("style");
    style.textContent = CSS;
    document.head.appendChild(style);

    var nav = document.createElement("nav");
    nav.className = "tabs";
    nav.setAttribute("aria-label", "Sections");

    // A tucked-away page shows a way back rather than pretending to be a top-level
    // destination — otherwise the only route home is the browser's back button.
    if (TUCKED.indexOf(here()) !== -1) {
      var back = document.createElement("a");
      back.className = "back";
      back.setAttribute("href", "/");
      back.textContent = "← Machines";
      nav.appendChild(back);
    }

    SECTIONS.forEach(function (s) {
      var a = document.createElement("a");
      a.setAttribute("href", s[0]);
      a.textContent = s[1];
      if (s[0] === here()) a.setAttribute("aria-current", "page");
      nav.appendChild(a);
    });

    var header = shell.querySelector("header.top");
    if (header && header.nextSibling) shell.insertBefore(nav, header.nextSibling);
    else if (header) shell.appendChild(nav);
    else shell.insertBefore(nav, shell.firstChild);
  }

  if (document.readyState === "loading") document.addEventListener("DOMContentLoaded", render);
  else render();
})();
