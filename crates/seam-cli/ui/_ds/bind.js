// seam — live data binding for the fleet page.
// Logic only. It clones the machine and row templates the design provides and fills
// them; it never invents structure, styling or copy. The design is the source of truth.
(function () {
  "use strict";
  var POLL_MS = 700;

  function short(id) { return String(id || "").slice(0, 8); }

  // Templates are captured once, before any data arrives, so repeated renders always
  // clone the authored markup rather than whatever the previous render left behind.
  var deskEl = document.querySelector(".desk");
  var machineTpl = null, selfTpl = null, deskEmpty = null;
  if (deskEl) {
    var machines = deskEl.querySelectorAll("[data-machine]");
    selfTpl = machines[0] ? machines[0].cloneNode(true) : null;
    for (var mi = 0; mi < machines.length; mi++) {
      if (!machines[mi].classList.contains("self")) { machineTpl = machines[mi].cloneNode(true); break; }
    }
    if (!machineTpl && selfTpl) { machineTpl = selfTpl.cloneNode(true); machineTpl.classList.remove("self"); }
    deskEmpty = deskEl.querySelector(".desk-empty");
    deskEmpty = deskEmpty ? deskEmpty.cloneNode(true) : null;
  }
  var peersEl = document.querySelector(".peers");
  var rowTpl = null;
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

  function renderDesk(s) {
    if (!deskEl || !machineTpl) return;
    deskEl.textContent = "";
    // This machine always appears, so the desk is never empty and a person can always
    // see where they are.
    var self = selfTpl.cloneNode(true);
    fillScreen(self, { name: s.name, size: short(s.id), edge: "this machine" }, s.focus === "local");
    deskEl.appendChild(self);

    (s.peers || []).forEach(function (p) {
      var el = machineTpl.cloneNode(true);
      fillScreen(el, {
        name: p.name || short(p.id),
        size: short(p.id) + " · " + (p.role || ""),
        edge: p.edge || "connected",
        enabled: p.enabled
      }, s.focus === short(p.id));
      deskEl.appendChild(el);
    });

    if (!(s.peers || []).length && deskEmpty) deskEl.appendChild(deskEmpty.cloneNode(true));
  }

  function renderPeers(s) {
    if (!peersEl || !rowTpl) return;
    var existing = peersEl.querySelectorAll("[data-peer-row]");
    for (var i = 0; i < existing.length; i++) existing[i].remove();
    var anchor = peersEl.firstChild;
    (s.peers || []).forEach(function (p) {
      var row = rowTpl.cloneNode(true);
      var holding = s.focus === short(p.id);
      var n = row.querySelector(".who .n"), d = row.querySelector(".who .id");
      if (n) n.textContent = p.name || short(p.id);
      if (d) d.textContent = short(p.id) + " · " + (p.role || "") + (p.edge ? " · " + p.edge + " edge" : "") + " · " + p.addr;
      var pill = row.querySelector(".pill");
      if (pill) {
        var cls = !p.enabled ? "offline" : (holding ? "holding" : "connected");
        pill.className = "pill " + cls;
        pill.innerHTML = '<span class="dot"></span>' + (!p.enabled ? "disabled" : (holding ? "holds pointer" : "connected"));
      }
      var sw = row.querySelector(".sw");
      if (sw) {
        sw.classList.toggle("on", !!p.enabled);
        sw.setAttribute("aria-checked", p.enabled ? "true" : "false");
        sw.setAttribute("data-peer", short(p.id));
      }
      peersEl.insertBefore(row, anchor);
    });
    var empty = document.querySelector(".peers-empty");
    if (empty) empty.style.display = (s.peers || []).length ? "none" : "";
  }

  function apply(s) {
    var mach = document.querySelector("header.top .machine");
    if (mach) mach.innerHTML = s.name + " · " + short(s.id) + " · <b>" + (s.role || "") + "</b><br>v" + s.version + " · " + s.platform;

    renderDesk(s);
    renderPeers(s);

    var xfer = document.querySelector(".xfer-now");
    if (xfer) {
      if (s.transfer) {
        xfer.style.display = "";
        var w = xfer.querySelector(".xfer-what"), dt = xfer.querySelector(".xfer-detail");
        if (w) w.textContent = s.transfer.what;
        if (dt) dt.textContent = s.transfer.detail;
        xfer.classList.toggle("busy", s.transfer.what.indexOf("sending") === 0);
      } else { xfer.style.display = "none"; }
    }

    var vals = document.querySelectorAll(".health .item .v");
    (s.health || []).forEach(function (h, i) {
      var v = vals[i]; if (!v) return;
      v.className = h.ok ? "v ok" : "v bad";
      v.textContent = (h.ok ? "● " : "○ ") + h.text;
    });

    if (s.shares) {
      [["text", s.shares.text], ["images", s.shares.images], ["files", s.shares.files]].forEach(function (pair) {
        var el = document.querySelector('[data-share="' + pair[0] + '"]');
        if (el) { el.classList.toggle("on", !!pair[1]); el.setAttribute("aria-checked", pair[1] ? "true" : "false"); }
      });
    }
    var su = document.querySelector("[data-startup]");
    if (su) { su.classList.toggle("on", !!s.startup); su.setAttribute("aria-checked", s.startup ? "true" : "false"); }

    var log = document.querySelector(".log-path");
    if (log) log.textContent = "log: INFO · port " + (s.port || "?");
  }

  function tick() {
    fetch("/state").then(function (r) { return r.json(); }).then(apply).catch(function () {});
  }
  function closeTab() { try { window.close(); } catch (ignored) {} }

  document.addEventListener("click", function (event) {
    var closest = event.target.closest ? event.target.closest.bind(event.target) : function () { return null; };
    var share = closest("[data-share]");
    if (share) {
      var kind = share.getAttribute("data-share");
      var on = share.getAttribute("aria-checked") !== "true";
      fetch("/action/share/" + kind + "/" + (on ? "on" : "off"), { method: "POST" }).then(tick).catch(function () {});
      return;
    }
    if (closest("[data-startup]")) {
      var el = closest("[data-startup]");
      var want = el.getAttribute("aria-checked") !== "true";
      fetch("/action/startup/" + (want ? "on" : "off"), { method: "POST" }).then(tick).catch(function () {});
      return;
    }
    var sw = closest("[data-peer]");
    if (sw) {
      var peer = sw.getAttribute("data-peer");
      var enable = sw.getAttribute("aria-checked") !== "true";
      fetch("/action/peer/" + peer + "/" + (enable ? "enable" : "disable"), { method: "POST" }).then(tick).catch(function () {});
      return;
    }
    if (closest("[data-action=release]")) {
      fetch("/action/release", { method: "POST" }).then(tick).catch(function () {});
    } else if (closest("[data-action=quit]")) {
      fetch("/action/quit", { method: "POST" }).then(closeTab, closeTab);
      var m = document.querySelector("header.top .machine");
      if (m) m.innerHTML = "seam stopped<br>launch the app to start again";
      setTimeout(closeTab, 400);
    }
  });

  tick();
  setInterval(tick, POLL_MS);
})();
