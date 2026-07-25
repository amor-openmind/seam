// seam — live data binding for the fleet page.
// Logic only: fills and toggles elements the design already specifies. It never
// creates, styles or repositions anything visible — the design is the source of truth.
(function () {
  "use strict";
  var POLL_MS = 1000;

  function short(id) { return String(id || "").slice(0, 8); }

  function apply(s) {
    // Header identity
    var mach = document.querySelector("header.top .machine");
    if (mach) mach.innerHTML = s.name + " · " + short(s.id) + "<br>v" + s.version + " · " + s.platform;

    // Desk focus — toggle the authored .holding class among the authored screens.
    var screens = { self: document.querySelector(".screen.mac"), imac: document.querySelector(".screen.imac"), laptop: document.querySelector(".screen.laptop") };
    var peerA = s.peers[0] ? short(s.peers[0].id) : null;
    var peerB = s.peers[1] ? short(s.peers[1].id) : null;
    Object.keys(screens).forEach(function (k) { if (screens[k]) screens[k].classList.remove("holding"); });
    if (s.focus === "local" && screens.self) screens.self.classList.add("holding");
    else if (s.focus === peerA && screens.imac) screens.imac.classList.add("holding");
    else if (s.focus === peerB && screens.laptop) screens.laptop.classList.add("holding");

    // Peers — rewrite the authored rows with live peers; hide unused rows.
    var rows = document.querySelectorAll(".peers .peer");
    for (var i = 0; i < rows.length; i++) {
      var row = rows[i], p = s.peers[i];
      if (!p) { row.style.display = "none"; continue; }
      row.style.display = "";
      var n = row.querySelector(".who .n"), d = row.querySelector(".who .id"), lat = row.querySelector(".lat");
      if (n) n.textContent = p.name || short(p.id);
      if (d) d.textContent = short(p.id) + " · " + p.addr;
      if (lat) lat.textContent = p.rtt_ms != null ? p.rtt_ms.toFixed(1) + " ms" : "";
      var pill = row.querySelector(".pill");
      if (pill) { pill.className = "pill connected"; pill.innerHTML = '<span class=\"dot\"></span>connected'; }
    }

    // Health — fill the authored items in order: capture, withhold, cursor, tap.
    var vals = document.querySelectorAll(".health .item .v");
    (s.health || []).forEach(function (h, i) {
      var v = vals[i]; if (!v) return;
      v.className = h.ok ? "v ok" : "v bad";
      v.textContent = (h.ok ? "● " : "○ ") + h.text;
    });
  }

  function tick() {
    fetch("/state").then(function (r) { return r.json(); }).then(apply).catch(function () {});
  }
  tick();
  setInterval(tick, POLL_MS);
})();
