// seam — the section navigation, authored here so every page shows the same one.
//
// The markup and styles below are the design's, kept in a single place rather than copied
// into nine pages where they would drift apart. A page that already carries its own nav
// keeps it untouched.
(function () {
  "use strict";

  var SECTIONS = [
    ["/", "Fleet"],
    ["/settings.html", "Settings"],
    ["/pairing.html", "Pairing"],
    ["/transfers.html", "Transfers"],
    ["/doctor.html", "Doctor"],
    ["/onboarding.html", "First run"],
    ["/update.html", "Update"]
  ];

  var CSS = "nav.tabs{display:flex;gap:2px;margin-bottom:20px;border-bottom:1px solid var(--border-subtle);overflow-x:auto}" +
    "nav.tabs a{padding:9px 13px;font:500 var(--text-sm)/1.2 var(--font-sans);color:var(--text-secondary);text-decoration:none;border-bottom:2px solid transparent;white-space:nowrap}" +
    "nav.tabs a:hover{color:var(--text-primary);background:var(--surface-hover)}" +
    "nav.tabs a[aria-current=page]{color:var(--text-link);border-bottom-color:var(--border-accent)}";

  function here() {
    return location.pathname === "" ? "/" : location.pathname;
  }

  function render() {
    var shell = document.querySelector(".shell");
    if (!shell) return;

    var nav = shell.querySelector("nav.tabs");
    if (!nav) {
      var style = document.createElement("style");
      style.textContent = CSS;
      document.head.appendChild(style);

      nav = document.createElement("nav");
      nav.className = "tabs";
      nav.setAttribute("aria-label", "Sections");
      SECTIONS.forEach(function (s) {
        var a = document.createElement("a");
        a.setAttribute("href", s[0]);
        a.textContent = s[1];
        nav.appendChild(a);
      });
      var header = shell.querySelector("header.top");
      if (header && header.nextSibling) shell.insertBefore(nav, header.nextSibling);
      else if (header) shell.appendChild(nav);
      else shell.insertBefore(nav, shell.firstChild);
    }

    var links = nav.querySelectorAll("a");
    for (var i = 0; i < links.length; i++) {
      if (links[i].getAttribute("href") === here()) links[i].setAttribute("aria-current", "page");
      else links[i].removeAttribute("aria-current");
    }
  }

  if (document.readyState === "loading") document.addEventListener("DOMContentLoaded", render);
  else render();
})();
