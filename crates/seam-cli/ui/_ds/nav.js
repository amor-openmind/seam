// seam — the section navigation.
//
// Every feature is reachable; none is hidden behind a mystery menu. The complexity comes
// out somewhere better: a machine only shows the sections that apply to IT.
//
// The machine seam was installed on first is the server — it shares its keyboard and mouse,
// it is where you add machines, and it is where the fleet is managed. A machine that joined
// is a client: it receives input, and "Add a machine" or "Pairing" on it would be offering
// a job that belongs to the other end. Roles are known from the first run, so the wrong
// screens never appear rather than appearing and disappointing.
(function () {
  "use strict";

  // [path, label, who] — "all", "server" or "client".
  var SECTIONS = [
    ["/", "Machines", "all"],
    ["/join.html", "Add a machine", "server"],
    ["/pairing.html", "Pairing", "server"],
    ["/transfers.html", "Clipboard", "all"],
    ["/settings.html", "Settings", "all"],
    ["/doctor.html", "Health", "all"],
    ["/update.html", "Update", "all"],
    ["/onboarding.html", "First run", "all"],
    ["/ideas.html", "Roadmap", "all"]
  ];

  var CSS = "nav.tabs{display:flex;align-items:center;gap:2px;margin-bottom:22px;" +
    "border-bottom:1px solid var(--border-subtle);overflow-x:auto}" +
    "nav.tabs a{padding:10px 13px;font:500 var(--text-sm)/1.2 var(--font-sans);color:var(--text-secondary);" +
    "text-decoration:none;border-bottom:2px solid transparent;white-space:nowrap}" +
    "nav.tabs a:hover{color:var(--text-primary);background:var(--surface-hover)}" +
    "nav.tabs a[aria-current=page]{color:var(--text-link);border-bottom-color:var(--border-accent)}" +
    "nav.tabs .role{margin-left:auto;padding-left:12px;flex:none;font:500 9px/1 var(--font-mono);" +
    "letter-spacing:.1em;text-transform:uppercase;color:var(--text-tertiary)}";

  function here() { return location.pathname === "" ? "/" : location.pathname; }

  function render(role) {
    var shell = document.querySelector(".shell");
    if (!shell) return;

    var old = shell.querySelector("nav.tabs");
    if (old) old.remove();

    if (!document.querySelector("style[data-seam-nav]")) {
      var style = document.createElement("style");
      style.setAttribute("data-seam-nav", "");
      style.textContent = CSS;
      document.head.appendChild(style);
    }

    var nav = document.createElement("nav");
    nav.className = "tabs";
    nav.setAttribute("aria-label", "Sections");

    SECTIONS.forEach(function (s) {
      // A section for the other role is skipped — unless it is the page being looked at,
      // which must never become unreachable from its own navigation.
      if (s[2] !== "all" && s[2] !== role && s[0] !== here()) return;
      var a = document.createElement("a");
      a.setAttribute("href", s[0]);
      a.textContent = s[1];
      if (s[0] === here()) a.setAttribute("aria-current", "page");
      nav.appendChild(a);
    });

    // Which end of the fleet this machine is, stated where it is always visible.
    var tag = document.createElement("span");
    tag.className = "role";
    tag.textContent = role === "server" ? "this machine shares input" : "receives input";
    nav.appendChild(tag);

    var header = shell.querySelector("header.top");
    if (header && header.nextSibling) shell.insertBefore(nav, header.nextSibling);
    else if (header) shell.appendChild(nav);
    else shell.insertBefore(nav, shell.firstChild);
  }

  // Render immediately from what is known, then again once the daemon answers: a nav that
  // waits for the network is a page that looks broken for a second on every load.
  render("server");
  if (window.seam && window.seam.onState) {
    window.seam.onState(function (s) {
      render((s.role || "").indexOf("shares") === 0 ? "server" : "client");
    });
  }
})();
