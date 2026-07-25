// seam — live data binding for the fleet page.
// Logic only: fills and toggles elements the design already specifies. It never
// creates, styles or repositions anything visible — the design is the source of truth.
(function () {
  "use strict";
  var POLL_MS = 700;

  function short(id) { return String(id || "").slice(0, 8); }

  // Peers map to desk slots by their PLACEMENT edge, never by list order: connection
  // order reshuffles on reconnect, and an order-based mapping highlighted the wrong
  // screen. A client has no layout of its own, so it falls back to name matching.
  function slotFor(p) {
    var byEdge = null;
    if (p.edge === "left" || p.edge === "right") byEdge = document.querySelector(".screen.imac");
    else if (p.edge === "bottom" || p.edge === "top") byEdge = document.querySelector(".screen.laptop");
    if (byEdge) return byEdge;
    var slots = document.querySelectorAll(".screen.imac, .screen.laptop, .screen.mac");
    for (var i = 0; i < slots.length; i++) {
      var size = slots[i].querySelector(".size");
      if (size && size.textContent.indexOf(short(p.id)) !== -1) return slots[i];
    }
    return null;
  }

  function apply(s) {
    var mach = document.querySelector("header.top .machine");
    if (mach) mach.innerHTML = s.name + " · " + short(s.id) + " · <b>" + (s.role || "") + "</b><br>v" + s.version + " · " + s.platform;

    // Highlight whichever screen actually holds the pointer. On a machine that shares
    // input this comes from its own focus; on one that only receives, "local" means
    // input is arriving here right now.
    var slots = document.querySelectorAll(".screen");
    for (var i = 0; i < slots.length; i++) slots[i].classList.remove("holding");
    var self = document.querySelector(".screen.mac");
    if (s.focus === "local" && self) self.classList.add("holding");
    (s.peers || []).forEach(function (p) {
      var slot = slotFor(p);
      if (!slot) return;
      slot.style.opacity = p.enabled ? "" : ".38";
      if (s.focus === short(p.id)) slot.classList.add("holding");
    });

    var rows = document.querySelectorAll(".peers .peer");
    for (var j = 0; j < rows.length; j++) {
      var row = rows[j], p = (s.peers || [])[j];
      if (!p) { row.style.display = "none"; continue; }
      row.style.display = "";
      var n = row.querySelector(".who .n"), d = row.querySelector(".who .id");
      if (n) n.textContent = p.name || short(p.id);
      if (d) d.textContent = short(p.id) + " · " + (p.role || "") + (p.edge ? " · " + p.edge + " edge" : "") + " · " + p.addr;
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
    var empty = document.querySelector(".peers-empty");
    if (empty) empty.style.display = (s.peers || []).length ? "none" : "";

    // Clipboard movement. Text is instant; an image or a folder is not, and a page that
    // shows nothing while megabytes move is indistinguishable from a broken one.
    var xfer = document.querySelector(".xfer-now");
    if (xfer) {
      if (s.transfer) {
        xfer.style.display = "";
        var w = xfer.querySelector(".xfer-what"), dt = xfer.querySelector(".xfer-detail");
        if (w) w.textContent = s.transfer.what;
        if (dt) dt.textContent = s.transfer.detail;
        xfer.classList.toggle("busy", s.transfer.what.indexOf("sending") === 0);
      } else {
        xfer.style.display = "none";
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

  function closeTab() { try { window.close(); } catch (ignored) {} }

  document.addEventListener("click", function (event) {
    var closest = event.target.closest ? event.target.closest.bind(event.target) : function () { return null; };
    var sw = closest(".peers .sw");
    if (sw) {
      var peer = sw.getAttribute("data-peer");
      var enable = sw.getAttribute("aria-checked") !== "true";
      if (peer) fetch("/action/peer/" + peer + "/" + (enable ? "enable" : "disable"), { method: "POST" }).then(tick).catch(function () {});
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
