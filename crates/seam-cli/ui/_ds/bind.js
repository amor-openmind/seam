// seam — live data for the machines page.
// Clones the templates the design provides and fills them. It never invents structure,
// styling or copy, and never leaves example content on screen.
(function () {
  "use strict";
  var POLL_MS = 700;

  function short(id) { return String(id || "").slice(0, 8); }

  var deskEl = document.querySelector(".desk");
  var selfTpl = null, machineTpl = null, deskEmpty = null;
  if (deskEl) {
    var machines = deskEl.querySelectorAll("[data-machine]");
    selfTpl = machines[0] ? machines[0].cloneNode(true) : null;
    for (var mi = 0; mi < machines.length; mi++) {
      if (!machines[mi].classList.contains("self")) { machineTpl = machines[mi].cloneNode(true); break; }
    }
    if (!machineTpl && selfTpl) { machineTpl = selfTpl.cloneNode(true); machineTpl.classList.remove("self"); }
    var de = deskEl.querySelector(".desk-empty");
    deskEmpty = de ? de.cloneNode(true) : null;
  }
  var peersEl = document.querySelector(".peers");
  var rowTpl = window.seamTemplate ? null : null;
  if (peersEl) {
    var firstRow = peersEl.querySelector("[data-peer-row]");
    rowTpl = firstRow ? firstRow.cloneNode(true) : null;
  }

  function fillScreen(el, m, holding) {
    var name = el.querySelector(".name"), size = el.querySelector(".size"), edge = el.querySelector(".edge");
    if (name) name.textContent = m.name;
    if (size) size.textContent = m.size;
    if (edge) edge.textContent = m.edge;
    el.classList.toggle("holding", !!holding);
    el.classList.toggle("off", m.enabled === false);
    el.setAttribute("aria-label", m.name + (holding ? ", holds the pointer" : ""));
  }

  function apply(s) {
    var mach = document.querySelector("header.top .machine");
    if (mach) mach.innerHTML = s.name + " · " + short(s.id) + " · <b>" + (s.role || "") + "</b><br>v" + s.version + " · " + s.platform;

    if (deskEl && machineTpl && selfTpl) {
      deskEl.textContent = "";
      var self = selfTpl.cloneNode(true);
      fillScreen(self, { name: s.name, size: short(s.id), edge: "this machine" }, s.focus === "local");
      deskEl.appendChild(self);
      (s.peers || []).forEach(function (p) {
        var el = machineTpl.cloneNode(true);
        fillScreen(el, {
          name: p.name || short(p.id),
          size: short(p.id),
          edge: p.edge || "connected",
          enabled: p.enabled
        }, s.focus === short(p.id));
        deskEl.appendChild(el);
      });
      if (!(s.peers || []).length && deskEmpty) deskEl.appendChild(deskEmpty.cloneNode(true));
    }

    if (peersEl && rowTpl) {
      var old = peersEl.querySelectorAll("[data-peer-row]");
      for (var i = 0; i < old.length; i++) old[i].remove();
      (s.peers || []).forEach(function (p) {
        var row = rowTpl.cloneNode(true);
        var holding = s.focus === short(p.id);
        var n = row.querySelector(".who .n"), d = row.querySelector(".who .id");
        if (n) n.textContent = p.name || short(p.id);
        if (d) d.textContent = short(p.id) + (p.edge ? " · " + p.edge + " edge" : "");
        var pill = row.querySelector(".pill");
        if (pill) {
          var cls = !p.enabled ? "offline" : (holding ? "holding" : "connected");
          pill.className = "pill " + cls;
          pill.innerHTML = '<span class="dot"></span>' + (!p.enabled ? "off" : (holding ? "has the pointer" : "connected"));
        }
        var sw = row.querySelector(".sw");
        if (sw) {
          sw.classList.toggle("on", !!p.enabled);
          sw.setAttribute("aria-checked", p.enabled ? "true" : "false");
          sw.setAttribute("data-peer", short(p.id));
        }
        peersEl.appendChild(row);
      });
      var empty = document.querySelector(".peers-empty");
      if (empty) empty.style.display = (s.peers || []).length ? "none" : "";
    }

    var xfer = document.querySelector(".xfer-now");
    if (xfer) {
      xfer.style.display = s.transfer ? "" : "none";
      if (s.transfer) {
        var w = xfer.querySelector(".xfer-what"), dt = xfer.querySelector(".xfer-detail");
        if (w) w.textContent = s.transfer.what;
        if (dt) dt.textContent = s.transfer.detail;
        xfer.classList.toggle("busy", s.transfer.what.indexOf("sending") === 0);
      }
    }

    // One line instead of four cards: everything is fine, or exactly what is not.
    var bad = (s.health || []).filter(function (h) { return !h.ok; });
    var line = document.querySelector("[data-health-line]");
    var text = document.querySelector("[data-health-text]");
    if (line) line.className = "status" + (bad.length ? " bad" : "");
    if (text) {
      text.textContent = bad.length
        ? (bad.length === 1 ? "One thing needs attention on this machine." : bad.length + " things need attention on this machine.")
        : "Everything is working on this machine.";
    }
  }

  function tick() {
    fetch("/state").then(function (r) { return r.json(); }).then(apply).catch(function () {});
  }

  document.addEventListener("click", function (event) {
    var closest = event.target.closest ? event.target.closest.bind(event.target) : function () { return null; };
    var sw = closest("[data-peer]");
    if (sw) {
      var peer = sw.getAttribute("data-peer");
      var enable = sw.getAttribute("aria-checked") !== "true";
      fetch("/action/peer/" + peer + "/" + (enable ? "enable" : "disable"), { method: "POST" }).then(tick).catch(function () {});
      return;
    }
    if (closest("[data-action=release]")) {
      fetch("/action/release", { method: "POST" }).then(tick).catch(function () {});
    }
  });

  tick();
  setInterval(tick, POLL_MS);
})();
