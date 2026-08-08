// Arrange your desk — each machine placed against ANOTHER machine, not against this one.
//
// The first model was a star: every machine on a side of this one. Real desks are chains.
// A laptop below the iMac, which is itself left of the Mac mini, cannot be described as
// "below the Mac mini" — and saying so put the pointer through the wrong edge.
//
// So a placement is a relation: <machine> is <side> of <neighbour>. The desk lays those
// relations out by walking from this machine outwards, which is why a chain draws as a
// chain and not as a ring around the middle.
(function () {
  "use strict";

  var EDGES = ["left", "right", "top", "bottom"];
  var OPPOSITE = { left: "right", right: "left", top: "bottom", bottom: "top" };
  var STEP = { left: [-1, 0], right: [1, 0], top: [0, -1], bottom: [0, 1] };

  var desk = document.querySelector("[data-desk]");
  var tpl = document.querySelector("[data-machine-template]");
  var rows = document.querySelector(".rows");
  var rowTpl = window.seam.template(rows, "[data-edge-row]");

  function save(peer, edge, anchor) {
    fetch("/action/layout/" + peer + "/" + edge + "/" + anchor, { method: "POST" })
      .then(function (r) { return r.json(); })
      .then(function (res) {
        if (!res.ok) return;
        var saved = document.querySelector("[data-saved]");
        if (saved) { saved.hidden = false; setTimeout(function () { saved.hidden = true; }, 1800); }
      })
      .catch(function () {});
  }

  // Walk the relations outward from this machine, giving every machine a grid coordinate.
  // A machine whose neighbour is not placed yet waits for a later pass; one that never
  // resolves is dropped rather than drawn somewhere arbitrary.
  function coordinates(s) {
    var self = window.seam.short(s.id);
    var at = {};
    at[self] = [0, 0];
    var pending = (s.peers || []).slice();
    for (var pass = 0; pass < 8 && pending.length; pass++) {
      pending = pending.filter(function (p) {
        var id = window.seam.short(p.id);
        var anchor = p.anchor && p.anchor !== "self" ? p.anchor : self;
        var edge = EDGES.indexOf(p.edge) === -1 ? "left" : p.edge;
        if (!at[anchor]) return true;
        var base = at[anchor];
        var d = STEP[edge];
        var x = base[0] + d[0], y = base[1] + d[1];
        // Never stack two machines on one square: shift along until the square is free.
        var guard = 0;
        while (Object.keys(at).some(function (k) { return at[k][0] === x && at[k][1] === y; })) {
          x += d[0] || 1; y += d[1];
          if (++guard > 8) return false;
        }
        at[id] = [x, y];
        return false;
      });
    }
    return at;
  }

  function render(s) {
    var peers = s.peers || [];
    var self = window.seam.short(s.id);
    var at = coordinates(s);

    if (desk && tpl) {
      var xs = Object.keys(at).map(function (k) { return at[k][0]; });
      var ys = Object.keys(at).map(function (k) { return at[k][1]; });
      var minX = Math.min.apply(null, xs) - 1, maxX = Math.max.apply(null, xs) + 1;
      var minY = Math.min.apply(null, ys) - 1, maxY = Math.max.apply(null, ys) + 1;

      desk.textContent = "";
      desk.style.gridTemplateColumns = "repeat(" + (maxX - minX + 1) + ",minmax(96px,150px))";
      desk.style.gridTemplateRows = "repeat(" + (maxY - minY + 1) + ",84px)";

      var byCoord = {};
      Object.keys(at).forEach(function (k) { byCoord[at[k][0] + "," + at[k][1]] = k; });

      for (var y = minY; y <= maxY; y++) {
        for (var x = minX; x <= maxX; x++) {
          var who = byCoord[x + "," + y];
          var cell = document.createElement("div");
          cell.className = "cell";
          cell.setAttribute("data-x", x);
          cell.setAttribute("data-y", y);

          if (!who) {
            cell.classList.add("empty");
            desk.appendChild(cell);
            continue;
          }

          var p = who === self ? null : peers.find(function (q) { return window.seam.short(q.id) === who; });
          var el = tpl.content.firstElementChild.cloneNode(true);
          if (!p) {
            el.classList.add("self");
            el.removeAttribute("draggable");
            window.seam.text(el, ".tag", "this machine");
            window.seam.text(el, ".name", s.name);
            window.seam.text(el, ".size", self);
            el.classList.toggle("holding", s.focus === "local");
          } else {
            var anchorName = p.anchor && p.anchor !== "self" ? p.anchor : self;
            window.seam.text(el, ".tag", (p.edge || "") + " of " + anchorName);
            window.seam.text(el, ".name", p.name || who);
            window.seam.text(el, ".size", who);
            el.setAttribute("data-peer", who);
            el.setAttribute("data-anchor-of", anchorName);
            el.classList.toggle("holding", s.focus === who);
          }
          cell.appendChild(el);
          desk.appendChild(cell);
        }
      }
    }

    // The written form of the same relation — folded away, and the only route usable
    // with a screen reader.
    if (rows && rowTpl) {
      window.seam.clear(rows, "[data-edge-row]");
      peers.forEach(function (p) {
        var id = window.seam.short(p.id);
        var row = rowTpl.cloneNode(true);
        window.seam.text(row, ".who .n", p.name || id);
        window.seam.text(row, ".who .d", id);

        var edgeSel = row.querySelector("[data-edge]");
        if (edgeSel) {
          if (EDGES.indexOf(p.edge) !== -1) edgeSel.value = p.edge;
          edgeSel.setAttribute("data-peer", id);
        }
        var anchorSel = row.querySelector("[data-anchor]");
        if (anchorSel) {
          anchorSel.textContent = "";
          var options = [[self, (s.name || "this machine") + " (this machine)"]];
          peers.forEach(function (q) {
            var qid = window.seam.short(q.id);
            if (qid !== id) options.push([qid, q.name || qid]);
          });
          options.forEach(function (o) {
            var opt = document.createElement("option");
            opt.value = o[0];
            opt.textContent = o[1];
            anchorSel.appendChild(opt);
          });
          anchorSel.value = p.anchor && p.anchor !== "self" ? p.anchor : self;
          anchorSel.setAttribute("data-peer", id);
        }
        rows.appendChild(row);
      });
      window.seam.show("[data-rows-empty]", !peers.length);
    }
  }

  window.seam.onState(render);

  // Dropping on an empty square means "next to whatever that square touches", so a chain
  // gets built by hand without anyone naming an anchor.
  var dragged = null;
  document.addEventListener("dragstart", function (e) {
    var el = e.target.closest && e.target.closest("[data-peer]");
    if (!el) return;
    dragged = el.getAttribute("data-peer");
    el.classList.add("dragging");
    // Empty squares are invisible until something is being carried: a desk full of dashed
    // boxes reads as a form, which is what this page is trying not to be.
    document.body.classList.add("dragging-now");
    if (e.dataTransfer) e.dataTransfer.effectAllowed = "move";
  });
  document.addEventListener("dragend", function () {
    document.body.classList.remove("dragging-now");
    var d = document.querySelector(".dragging");
    if (d) d.classList.remove("dragging");
    var o = document.querySelector(".cell.over");
    if (o) o.classList.remove("over");
    dragged = null;
  });
  document.addEventListener("dragover", function (e) {
    if (!dragged) return;
    var c = e.target.closest && e.target.closest("[data-x]");
    if (!c || !c.classList.contains("empty")) return;
    e.preventDefault();
    var prev = document.querySelector(".cell.over");
    if (prev && prev !== c) prev.classList.remove("over");
    c.classList.add("over");
  });
  document.addEventListener("drop", function (e) {
    if (!dragged) return;
    var c = e.target.closest && e.target.closest("[data-x]");
    if (!c || !c.classList.contains("empty")) return;
    e.preventDefault();
    var x = Number(c.getAttribute("data-x")), y = Number(c.getAttribute("data-y"));

    var found = null;
    EDGES.forEach(function (edge) {
      if (found) return;
      var d = STEP[edge];
      var n = document.querySelector('[data-x="' + (x + d[0]) + '"][data-y="' + (y + d[1]) + '"]');
      var occupant = n && n.querySelector(".screen");
      if (!occupant) return;
      var anchor = occupant.classList.contains("self")
        ? occupant.querySelector(".size").textContent
        : occupant.getAttribute("data-peer");
      if (anchor && anchor !== dragged) found = { edge: OPPOSITE[edge], anchor: anchor };
    });

    if (found) save(dragged, found.edge, found.anchor);
    dragged = null;
  });

  // Arrow keys move a focused machine around the machine it is already beside, so the
  // keyboard reaches every arrangement the mouse can.
  document.addEventListener("keydown", function (e) {
    var el = e.target.closest && e.target.closest("[data-peer]");
    if (!el) return;
    var map = { ArrowLeft: "left", ArrowRight: "right", ArrowUp: "top", ArrowDown: "bottom" };
    if (!map[e.key]) return;
    e.preventDefault();
    save(el.getAttribute("data-peer"), map[e.key], el.getAttribute("data-anchor-of") || "self");
  });

  document.addEventListener("change", function (e) {
    var el = e.target.closest && e.target.closest("[data-edge], [data-anchor]");
    if (!el) return;
    var row = el.closest("[data-edge-row]");
    var peer = el.getAttribute("data-peer");
    var edge = row.querySelector("[data-edge]");
    var anchor = row.querySelector("[data-anchor]");
    if (peer && edge && anchor) save(peer, edge.value, anchor.value);
  });
})();
