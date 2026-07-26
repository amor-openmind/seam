// Arrange your desk — machines placed where they sit, not listed.
//
// The first version appended every machine to one container, so a page for arranging
// things showed a column. The design provides a three-by-three desk with a named cell per
// side; this puts each machine in the cell matching its edge, which is the whole point.
//
// Both surfaces write the same thing: dropping a machine on a side, and choosing that side
// by name. The named control is not a lesser fallback — it is the only one that works with
// a keyboard, and it is what the drag is shorthand for.
(function () {
  "use strict";

  var EDGES = ["left", "right", "top", "bottom"];
  var desk = document.querySelector("[data-desk]");
  var tpl = document.querySelector("[data-machine-template]");
  var rows = document.querySelector(".rows");
  var rowTpl = window.seam.template(rows, "[data-edge-row]");

  function cell(side) { return document.querySelector('[data-cell="' + side + '"]'); }

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

  function render(s) {
    var peers = s.peers || [];

    var self = document.querySelector("[data-self]");
    if (self) {
      window.seam.text(self, ".name", s.name);
      window.seam.text(self, ".size", window.seam.short(s.id));
      self.classList.toggle("holding", s.focus === "local");
    }

    if (desk && tpl) {
      // A machine can only occupy one side, so the first claim on a side wins and any
      // other lands on the first free one. Two machines drawn on top of each other would
      // be worse than one of them being somewhere it was not put.
      var taken = {};
      EDGES.forEach(function (side) {
        var c = cell(side);
        if (!c) return;
        c.textContent = "";
        c.className = "cell " + side + " empty";
        c.textContent = side === "top" ? "above" : side === "bottom" ? "below" : side;
      });

      peers.forEach(function (p) {
        var side = EDGES.indexOf(p.edge) === -1 ? null : p.edge;
        if (!side || taken[side]) side = EDGES.find(function (e) { return !taken[e]; });
        if (!side) return;
        taken[side] = true;

        var c = cell(side);
        if (!c) return;
        c.textContent = "";
        c.className = "cell " + side + " touching";

        var el = tpl.content.firstElementChild.cloneNode(true);
        var short = window.seam.short(p.id);
        window.seam.text(el, ".tag", side === "top" ? "above" : side === "bottom" ? "below" : side);
        window.seam.text(el, ".name", p.name || short);
        window.seam.text(el, ".size", short);
        el.setAttribute("data-peer", short);
        el.classList.toggle("holding", s.focus === short);
        c.appendChild(el);
      });
    }

    if (rows && rowTpl) {
      window.seam.clear(rows, "[data-edge-row]");
      peers.forEach(function (p) {
        var row = rowTpl.cloneNode(true);
        var short = window.seam.short(p.id);
        window.seam.text(row, ".who .n", p.name || short);
        window.seam.text(row, ".who .d", short);
        var sel = row.querySelector("[data-edge]");
        if (sel) {
          if (EDGES.indexOf(p.edge) !== -1) sel.value = p.edge;
          sel.setAttribute("data-peer", short);
        }
        rows.appendChild(row);
      });
      window.seam.show("[data-rows-empty]", !peers.length);
    }
  }

  window.seam.onState(render);

  // Dragging: the cell dropped on IS the answer, so there is nothing to infer from
  // coordinates and no corner case at the corners.
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
    var over = document.querySelector(".cell.over");
    if (over) over.classList.remove("over");
    dragged = null;
  });
  document.addEventListener("dragover", function (e) {
    if (!dragged) return;
    var c = e.target.closest && e.target.closest("[data-cell]");
    if (!c || c.getAttribute("data-cell") === "self") return;
    e.preventDefault();
    var prev = document.querySelector(".cell.over");
    if (prev && prev !== c) prev.classList.remove("over");
    c.classList.add("over");
  });
  document.addEventListener("drop", function (e) {
    if (!dragged) return;
    var c = e.target.closest && e.target.closest("[data-cell]");
    if (!c) return;
    var side = c.getAttribute("data-cell");
    if (side === "self" || EDGES.indexOf(side) === -1) return;
    e.preventDefault();
    c.classList.remove("over");
    place(dragged, side);
    dragged = null;
  });

  document.addEventListener("change", function (e) {
    var sel = e.target.closest && e.target.closest("[data-edge]");
    if (!sel) return;
    var peer = sel.getAttribute("data-peer");
    if (peer && EDGES.indexOf(sel.value) !== -1) place(peer, sel.value);
  });

  // The keyboard route: arrow keys on a focused machine put it on that side.
  document.addEventListener("keydown", function (e) {
    var el = e.target.closest && e.target.closest("[data-peer]");
    if (!el) return;
    var map = { ArrowLeft: "left", ArrowRight: "right", ArrowUp: "top", ArrowDown: "bottom" };
    if (!map[e.key]) return;
    e.preventDefault();
    place(el.getAttribute("data-peer"), map[e.key]);
  });
})();
