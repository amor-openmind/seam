// Arrange your desk — bound to the arrangement the daemon actually stores.
//
// Both surfaces the design provides write the same thing: dragging a screen to a side of
// this machine, and choosing that side by name. The named control is not a lesser
// fallback — it is the only one that works with a keyboard, and it is what the drag is
// shorthand for.
(function () {
  "use strict";

  var desk = document.querySelector("[data-desk]");
  var selfTpl = window.seam.template(desk, "[data-machine].self");
  var machineTpl = window.seam.template(desk, "[data-machine]:not(.self)") ||
    (selfTpl ? selfTpl.cloneNode(true) : null);
  if (machineTpl) machineTpl.classList.remove("self");

  var rows = document.querySelector(".rows");
  var rowTpl = window.seam.template(rows, "[data-edge-row]");

  var EDGES = ["left", "right", "top", "bottom"];

  function place(peer, edge) {
    fetch("/action/layout/" + peer + "/" + edge, { method: "POST" })
      .then(function (r) { return r.json(); })
      .then(function (res) {
        if (!res.ok) return;
        var saved = document.querySelector("[data-saved]");
        if (saved) {
          saved.hidden = false;
          setTimeout(function () { saved.hidden = true; }, 1800);
        }
      })
      .catch(function () {});
  }

  // Which side of this machine a dropped screen landed on, from the pointer position
  // relative to the centre of this machine's screen. The larger offset wins, so a drop
  // near a corner resolves to the side it is most clearly on rather than to nothing.
  function sideOf(selfRect, x, y) {
    var dx = x - (selfRect.left + selfRect.width / 2);
    var dy = y - (selfRect.top + selfRect.height / 2);
    if (Math.abs(dx) > Math.abs(dy)) return dx < 0 ? "left" : "right";
    return dy < 0 ? "top" : "bottom";
  }

  function render(s) {
    var peers = s.peers || [];

    if (desk && selfTpl && machineTpl) {
      desk.textContent = "";
      var self = selfTpl.cloneNode(true);
      window.seam.text(self, ".name", s.name);
      window.seam.text(self, ".size", window.seam.short(s.id));
      self.removeAttribute("draggable");
      self.style.cursor = "default";
      desk.appendChild(self);

      peers.forEach(function (p) {
        var el = machineTpl.cloneNode(true);
        window.seam.text(el, ".name", p.name || window.seam.short(p.id));
        window.seam.text(el, ".size", window.seam.short(p.id) + (p.edge ? " · " + p.edge : ""));
        el.setAttribute("data-peer", window.seam.short(p.id));
        el.classList.toggle("holding", s.focus === window.seam.short(p.id));
        desk.appendChild(el);
      });
    }

    if (rows && rowTpl) {
      window.seam.clear(rows, "[data-edge-row]");
      peers.forEach(function (p) {
        var row = rowTpl.cloneNode(true);
        window.seam.text(row, ".who .n", p.name || window.seam.short(p.id));
        window.seam.text(row, ".who .d", window.seam.short(p.id));
        var sel = row.querySelector("[data-edge]");
        if (sel) {
          sel.value = EDGES.indexOf(p.edge) === -1 ? "none" : p.edge;
          sel.setAttribute("data-peer", window.seam.short(p.id));
        }
        rows.appendChild(row);
      });
      window.seam.show("[data-rows-empty]", !peers.length);
    }
  }

  window.seam.onState(render);

  // Dragging.
  var dragged = null;
  document.addEventListener("dragstart", function (e) {
    var el = e.target.closest && e.target.closest("[data-peer]");
    if (!el) return;
    dragged = el.getAttribute("data-peer");
    el.classList.add("dragging");
    if (e.dataTransfer) e.dataTransfer.effectAllowed = "move";
  });
  document.addEventListener("dragend", function (e) {
    var el = e.target.closest && e.target.closest("[data-peer]");
    if (el) el.classList.remove("dragging");
    dragged = null;
  });
  document.addEventListener("dragover", function (e) {
    if (dragged) e.preventDefault();
  });
  document.addEventListener("drop", function (e) {
    if (!dragged) return;
    e.preventDefault();
    var self = document.querySelector("[data-machine].self");
    if (!self) return;
    place(dragged, sideOf(self.getBoundingClientRect(), e.clientX, e.clientY));
    dragged = null;
  });

  // Choosing by name — and the keyboard route through the desk itself.
  document.addEventListener("change", function (e) {
    var sel = e.target.closest && e.target.closest("[data-edge]");
    if (!sel) return;
    var peer = sel.getAttribute("data-peer");
    if (peer && EDGES.indexOf(sel.value) !== -1) place(peer, sel.value);
  });
  document.addEventListener("keydown", function (e) {
    var el = e.target.closest && e.target.closest("[data-peer]");
    if (!el) return;
    var map = { ArrowLeft: "left", ArrowRight: "right", ArrowUp: "top", ArrowDown: "bottom" };
    if (!map[e.key]) return;
    e.preventDefault();
    place(el.getAttribute("data-peer"), map[e.key]);
  });
})();
