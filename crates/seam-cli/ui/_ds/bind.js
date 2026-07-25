// seam — live data binding for the fleet page.
// Logic only: fills and toggles elements the design already specifies. It never
// creates, styles or repositions anything visible — the design is the source of truth.
(function () {
  "use strict";
  var POLL_MS = 1000;

  function short(id) { return String(id || "").slice(0, 8); }

  // The authored desk has three slots: the left-edge machine, this machine, and the
  // machine below. Peers are matched to slots by their PLACEMENT (the edge the daemon
  // reports), never by list order — connection order reshuffles on reconnect, and an
  // order-based mapping highlighted the wrong screen.
  function slotFor(edge) {
    if (edge === "left" || edge === "right") return document.querySelector(".screen.imac");
    if (edge === "bottom" || edge === "top") return document.querySelector(".screen.laptop");
    return null;
  }

  function apply(s) {
    var mach = document.querySelector("header.top .machine");
    if (mach) mach.innerHTML = s.name + " · " + short(s.id) + "<br>v" + s.version + " · " + s.platform;

    var self = document.querySelector(".screen.mac");
    if (self) self.classList.toggle("holding", s.focus === "local");
    ["imac", "laptop"].forEach(function (k) {
      var el = document.querySelector(".screen." + k);
      if (el) el.classList.remove("holding");
    });
    (s.peers || []).forEach(function (p) {
      var slot = slotFor(p.edge);
      if (!slot) return;
      var name = slot.querySelector(".name");
      if (name && name.childNodes.length && p.name) name.childNodes[0].nodeValue = p.name === short(p.id) ? name.childNodes[0].nodeValue : p.name;
      if (s.focus === short(p.id)) slot.classList.add("holding");
    });

    var rows = document.querySelectorAll(".peers .peer");
    for (var i = 0; i < rows.length; i++) {
      var row = rows[i], p = (s.peers || [])[i];
      if (!p) { row.style.display = "none"; continue; }
      row.style.display = "";
      var n = row.querySelector(".who .n"), d = row.querySelector(".who .id"), lat = row.querySelector(".lat");
      if (n) n.textContent = p.name || short(p.id);
      if (d) d.textContent = short(p.id) + " · " + (p.edge ? p.edge + " edge · " : "") + p.addr;
      if (lat) lat.textContent = "";
      var pill = row.querySelector(".pill");
      if (pill) { pill.className = "pill " + (s.focus === short(p.id) ? "holding" : "connected"); pill.innerHTML = '<span class=\"dot\"></span>' + (s.focus === short(p.id) ? "holds pointer" : "connected"); }
    }

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

  document.addEventListener("click", function (event) {
    var btn = event.target.closest && event.target.closest("[data-action=release]");
    if (!btn) return;
    fetch("/action/release", { method: "POST" }).then(tick).catch(function () {});
  });

  tick();
  setInterval(tick, POLL_MS);
})();
