// Update — reacts to whatever the daemon last saw on the downloads page.
//
// The daemon does the looking: a browser cannot read the releases API without exposing
// this page to the network, and the daemon already polls on a schedule. This only renders
// what it reports, and shows nothing rather than guessing when it has not checked yet.
(function () {
  "use strict";

  var fleet = document.querySelector(".fleet");
  var rowTpl = window.seam.template(fleet, "[data-up-machine]");

  function setAll(selector, value) {
    var all = document.querySelectorAll(selector);
    for (var i = 0; i < all.length; i++) all[i].textContent = value;
  }

  window.seam.onState(function (s) {
    var running = "v" + s.version;
    setAll("[data-up-running]", running);

    var u = s.update || null;
    var newer = !!(u && u.latest && u.latest !== s.version);

    window.seam.show("[data-up-current]", !!u && !newer);
    window.seam.show("[data-up-available]", newer);

    if (newer) {
      setAll("[data-up-latest]", "v" + u.latest);
      window.seam.text(document, "[data-up-notes]", u.notes || "");
      window.seam.text(document, "[data-up-age]", u.published ? "published " + u.published : "");
      var dl = document.querySelector("[data-up-download]");
      var page = document.querySelector("[data-up-page]");
      if (dl && u.asset) dl.setAttribute("href", u.asset);
      if (page && u.page) page.setAttribute("href", u.page);
    }

    window.seam.text(document, "[data-up-checked]",
      u && u.checked ? "last checked " + u.checked : "not checked yet");

    // The fleet: every connected machine and whether it matches this one.
    if (fleet && rowTpl) {
      window.seam.clear(fleet, "[data-up-machine]");
      (s.peers || []).forEach(function (p) {
        var row = rowTpl.cloneNode(true);
        var behind = p.version && p.version !== s.version;
        var pill = row.querySelector(".pill");
        if (pill) {
          pill.className = "pill " + (behind ? "degraded" : "connected");
          pill.innerHTML = '<span class="dot"></span>' + (behind ? "behind" : "up to date");
        }
        window.seam.text(row, ".nm", p.name || window.seam.short(p.id));
        window.seam.text(row, ".v", p.version ? "v" + p.version : "version unknown");
        fleet.appendChild(row);
      });
      window.seam.show("[data-up-fleet-empty]", !(s.peers || []).length);
    }
  });
})();
