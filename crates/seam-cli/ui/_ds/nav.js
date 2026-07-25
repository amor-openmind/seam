// seam — the section navigation, authored here so every page shows the same one.
//
// The markup and styles below are the design's, kept in a single place rather than copied
// into a dozen pages where they would drift apart. A page that already carries its own nav
// keeps it untouched.
(function () {
  "use strict";

  // Order is the order a person meets them: the fleet first, then the two things they
  // came to do (add a machine, pair it), then the things they consult.
  var SECTIONS = [
    ["/", "Fleet"],
    ["/join.html", "Add a machine"],
    ["/pairing.html", "Pairing"],
    ["/settings.html", "Settings"],
    ["/transfers.html", "Clipboard"],
    ["/doctor.html", "Doctor"],
    ["/update.html", "Update"],
    ["/onboarding.html", "First run"],
    ["/ideas.html", "Roadmap"]
  ];

  var CSS = "nav.tabs{display:flex;gap:2px;margin-bottom:20px;border-bottom:1px solid var(--border-subtle);overflow-x:auto}" +
    "nav.tabs a{padding:9px 13px;font:500 var(--text-sm)/1.2 var(--font-sans);color:var(--text-secondary);text-decoration:none;border-bottom:2px solid transparent;white-space:nowrap}" +
    "nav.tabs a:hover{color:var(--text-primary);background:var(--surface-hover)}" +
    "nav.tabs a[aria-current=page]{color:var(--text-link);border-bottom-color:var(--border-accent)}";

  function here() {
    return location.pathname === "" ? "/" : location.pathname;
  }

  function build(shell) {
    var style = document.createElement("style");
    style.textContent = CSS;
    document.head.appendChild(style);

    var nav = document.createElement("nav");
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
    return nav;
  }

  function render() {
    var shell = document.querySelector(".shell");
    if (!shell) return;

    // A page carrying its own nav from an earlier design is replaced, so the set of
    // sections is defined in exactly one place and cannot fall behind.
    var existing = shell.querySelector("nav.tabs");
    if (existing) existing.remove();
    var nav = build(shell);

    var links = nav.querySelectorAll("a");
    for (var i = 0; i < links.length; i++) {
      if (links[i].getAttribute("href") === here()) links[i].setAttribute("aria-current", "page");
      else links[i].removeAttribute("aria-current");
    }
  }

  if (document.readyState === "loading") document.addEventListener("DOMContentLoaded", render);
  else render();
})();
