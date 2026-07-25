// seam — the live line: what just happened, on every page.
//
// Added back after the three-section simplification removed the activity feed entirely.
// That left both machines silent during the one minute a person most needs feedback —
// waiting for a machine to join — and silence is indistinguishable from nothing working.
//
// One line, not a log: the most recent thing, with a quiet count of what came before.
(function () {
  "use strict";

  var CSS = ".seam-live{display:flex;align-items:center;gap:10px;margin-top:14px;padding:11px 14px;" +
    "border-radius:var(--radius-md);background:var(--surface-sunken);" +
    "font:400 var(--text-xs)/1.5 var(--font-sans);color:var(--text-secondary)}" +
    ".seam-live .t{font-family:var(--font-mono);font-size:var(--text-2xs);color:var(--text-tertiary);flex:none}" +
    ".seam-live .w{flex:1;min-width:0;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}" +
    ".seam-live .pulse{width:7px;height:7px;border-radius:50%;background:var(--sage-500);flex:none}" +
    ".seam-live.waiting .pulse{background:var(--amber-500);animation:seam-pulse 1.4s ease-in-out infinite}" +
    "@keyframes seam-pulse{0%,100%{opacity:1}50%{opacity:.35}}";

  var el = null;

  function ensure() {
    if (el) return el;
    var shell = document.querySelector(".shell");
    if (!shell) return null;
    var style = document.createElement("style");
    style.textContent = CSS;
    document.head.appendChild(style);

    el = document.createElement("div");
    el.className = "seam-live";
    el.setAttribute("role", "status");
    el.setAttribute("aria-live", "polite");
    el.innerHTML = '<span class="pulse"></span><span class="t"></span><span class="w"></span>';
    shell.appendChild(el);
    return el;
  }

  window.seam.onState(function (s) {
    var node = ensure();
    if (!node) return;

    var recent = (s.activity || [])[0];
    // Waiting for a machine is a state worth showing as a state, not as absence.
    var waiting = !(s.peers || []).length;
    node.classList.toggle("waiting", waiting);

    var when = node.querySelector(".t");
    var what = node.querySelector(".w");
    if (waiting && !recent) {
      if (when) when.textContent = "";
      if (what) what.textContent = "Listening for machines on this network…";
      return;
    }
    if (!recent) {
      if (when) when.textContent = "";
      if (what) what.textContent = "Nothing has happened yet.";
      return;
    }
    if (when) when.textContent = recent.time;
    if (what) {
      what.textContent = waiting
        ? recent.what + " · waiting for a machine to join…"
        : recent.what;
    }
  });
})();
