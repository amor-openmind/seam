// seam — the stopped state, authored here because it is a screen the user sees.
//
// Closing the tab is best effort and usually refused: browsers only allow window.close()
// on a tab with no history, so the moment someone uses the section tabs the tab can no
// longer close itself. Relying on it left the page sitting there looking alive after the
// daemon had exited, which is worse than not closing at all — so the page says plainly
// that seam has stopped, and tries to close as a bonus rather than as the plan.
(function () {
  "use strict";

  var CSS = ".seam-stopped{position:fixed;inset:0;z-index:99;display:flex;align-items:center;justify-content:center;" +
    "background:var(--surface-page);padding:24px}" +
    ".seam-stopped .box{max-width:34ch;text-align:center}" +
    ".seam-stopped h2{font:400 var(--text-2xl)/1.15 var(--font-display);margin-bottom:10px}" +
    ".seam-stopped p{font:400 var(--text-sm)/1.6 var(--font-sans);color:var(--text-secondary);margin-bottom:6px}" +
    ".seam-stopped .hint{font:400 var(--text-xs)/1.6 var(--font-mono);color:var(--text-tertiary);margin-top:14px}";

  function showStopped(reason) {
    if (document.querySelector(".seam-stopped")) return;
    var style = document.createElement("style");
    style.textContent = CSS;
    document.head.appendChild(style);

    var overlay = document.createElement("div");
    overlay.className = "seam-stopped";
    overlay.setAttribute("role", "status");
    overlay.setAttribute("aria-live", "polite");

    var box = document.createElement("div");
    box.className = "box";

    var h = document.createElement("h2");
    var p = document.createElement("p");
    var q = document.createElement("p");
    if (reason === "replaced") {
      h.textContent = "This page is out of date";
      p.textContent = "A newer seam took over on this machine.";
      q.textContent = "Its own page is already open — this tab can be closed.";
    } else if (reason === "updating") {
      h.textContent = "Downloading the update";
      p.textContent = "seam has stopped so the new version can replace it.";
      q.textContent = "Run the download when it finishes; it will open its own page.";
    } else {
      h.textContent = "seam has stopped";
      p.textContent = "Your mouse, keyboard and clipboard are back on this machine only.";
      q.textContent = "Launch seam again to share them.";
    }
    var hint = document.createElement("div");
    hint.className = "hint";
    hint.textContent = "this tab can be closed";

    box.appendChild(h);
    box.appendChild(p);
    box.appendChild(q);
    box.appendChild(hint);
    overlay.appendChild(box);
    document.body.appendChild(overlay);
  }

  window.seamStopped = showStopped;

  function tryClose() {
    // Best effort only. Allowed for a tab the daemon itself opened and never navigated;
    // refused once the section tabs have been used, which is why the message is the real
    // answer rather than the fallback.
    setTimeout(function () {
      try { window.close(); } catch (ignored) { /* the message stands */ }
    }, 250);
  }

  window.seamQuit = function (reason) {
    fetch("/action/quit", { method: "POST" }).catch(function () {});
    showStopped(reason);
    tryClose();
  };

  document.addEventListener("click", function (event) {
    var closest = event.target.closest ? event.target.closest.bind(event.target) : function () { return null; };
    if (closest("[data-action=quit]")) {
      event.preventDefault();
      window.seamQuit();
      return;
    }
    // Downloading an update: stop seam first so the running copy is not holding the port
    // or the input tap when the new one starts. The download itself continues in the
    // browser — only this tab and the old daemon go away.
    if (closest("[data-up-download]")) {
      window.seamQuit("updating");
    }
  });

  // A page whose daemon has been replaced should say so rather than sit there polling a
  // port that now belongs to a different seam.
  window.addEventListener("seam:replaced", function () { showStopped("replaced"); });
})();
