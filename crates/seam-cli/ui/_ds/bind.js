// seam — live data binding for the fleet page.
// Logic only: fills and toggles elements the design already specifies. It never
// creates, styles or repositions anything visible — the design is the source of truth.
(function () {
  "use strict";
  var POLL_MS = 1000;

  function short(id) { return String(id || "").slice(0, 8); }

  function slotFor(edge) {
    if (edge === "left" || edge === "right") return document.querySelector(".screen.imac");
    if (edge === "bottom" || edge === "top") return document.querySelector(".screen.laptop");
    return null;
  }

  function apply(s) {
    var mach = document.querySelector("header.top .machine");
    if (mach) mach.innerHTML = s.name + " · " + short(s.id) + " · <b>" + (s.role || "server") + "</b> · port " + (s.port || "?") + "<br>v" + s.version + " · " + s.platform;

    var self = document.querySelector(".screen.mac");
    if (self) self.classList.toggle("holding", s.focus === "local");
    ["imac", "laptop"].forEach(function (k) {
      var el = document.querySelector(".screen." + k);
      if (el) el.classList.remove("holding");
    });
    (s.peers || []).forEach(function (p) {
      var slot = slotFor(p.edge);
      if (!slot) return;
      slot.style.opacity = p.enabled ? "" : ".38";
      if (p.enabled && s.focus === short(p.id)) slot.classList.add("holding");
    });

    var rows = document.querySelectorAll(".peers .peer");
    for (var i = 0; i < rows.length; i++) {
      var row = rows[i], p = (s.peers || [])[i];
      if (!p) { row.style.display = "none"; continue; }
      row.style.display = "";
      var n = row.querySelector(".who .n"), d = row.querySelector(".who .id");
      if (n) n.textContent = p.name || short(p.id);
      if (d) d.textContent = short(p.id) + " · " + (p.role || "client") + " · " + (p.edge ? p.edge + " edge · " : "") + p.addr;
      var pill = row.querySelector(".pill");
      if (pill) {
        var cls = !p.enabled ? "offline" : (s.focus === short(p.id) ? "holding" : "connected");
        var txt = !p.enabled ? "disabled" : (s.focus === short(p.id) ? "holds pointer" : "connected");
        pill.className = "pill " + cls;
        pill.innerHTML = '<span class=\"dot\"></span>' + txt;
      }
      var sw = row.querySelector(".sw");
      if (sw) {
        sw.classList.toggle("on", !!p.enabled);
        sw.setAttribute("aria-checked", p.enabled ? "true" : "false");
        sw.setAttribute("data-peer", short(p.id));
      }
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

  function closeTab() {
    try { window.close(); } catch (ignored) {}
  }

  document.addEventListener("click", function (event) {
    var closest = event.target.closest ? event.target.closest.bind(event.target) : function () { return null; };
    var sw = closest(".peers .sw");
    if (sw) {
      var peer = sw.getAttribute("data-peer");
      var enable = sw.getAttribute("aria-checked") !== "true";
      if (peer) {
        fetch("/action/peer/" + peer + "/" + (enable ? "enable" : "disable"), { method: "POST" })
          .then(tick)
          .catch(function () {});
      }
      return;
    }
    if (closest("[data-action=release]")) {
      fetch("/action/release", { method: "POST" }).then(tick).catch(function () {});
    } else if (closest("[data-action=quit]")) {
      fetch("/action/quit", { method: "POST" }).then(closeTab, closeTab);
      var mach = document.querySelector("header.top .machine");
      if (mach) mach.innerHTML = "seam stopped<br>launch the app to start again";
      setTimeout(closeTab, 400);
    }
  });

  tick();
  setInterval(tick, POLL_MS);
})();
