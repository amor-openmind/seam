// Pair a machine — the six-digit moment, driven by the daemon.
//
// The page's HTML is a state catalogue from the design: discovering, machines found,
// verify, paired, numbers differ. In the product exactly one state is visible at a time,
// chosen by what the daemon reports — the same collapse every other bound page performs.
//
// Nothing here decides anything about trust. The buttons post intentions; the daemon
// holds the parked link, derives the code from the encrypted channel, and writes trust
// only on confirm. The page is a mirror with two buttons.
(function () {
  "use strict";

  var states = {};
  document.querySelectorAll(".state").forEach(function (block) {
    var label = block.querySelector(":scope > .label");
    var key = label ? label.textContent.replace("state ·", "").trim() : "";
    if (label) label.style.display = "none";
    states[key] = block;
  });

  function show(keys) {
    Object.keys(states).forEach(function (key) {
      states[key].style.display = keys.indexOf(key) === -1 ? "none" : "";
    });
  }

  function digits(code) {
    return code.split("").join(" ");
  }

  function renderFound(s) {
    var list = states["machines found"];
    if (!list) return;
    var wrap = list.querySelector(".found");
    if (!wrap) return;
    var row = wrap.querySelector(".m");
    if (!row) return;
    if (!wrap.dataset.template) {
      wrap.dataset.template = "1";
      wrap.templateRow = row.cloneNode(true);
    }
    wrap.replaceChildren();
    var everyone = [];
    (s.peers || []).forEach(function (p) {
      everyone.push({ id: p.id, note: p.id + " · connected", paired: true });
    });
    (s.discovered || []).forEach(function (d) {
      var already = everyone.some(function (m) { return m.id === d.id; });
      if (!already && d.id !== window.seam.short(s.id)) {
        everyone.push({ id: d.id, note: d.id + " · " + d.addr, paired: d.trusted });
      }
    });
    everyone.push({ id: window.seam.short(s.id), note: s.id + " · this machine", you: true });
    everyone.forEach(function (m) {
      var el = wrap.templateRow.cloneNode(true);
      var name = el.querySelector(".who .n");
      var detail = el.querySelector(".who .d");
      if (name) name.textContent = m.id;
      if (detail) detail.textContent = m.note;
      var button = el.querySelector("button");
      var pill = el.querySelector(".pill");
      if (m.you) {
        el.style.opacity = ".55";
        if (button) button.remove();
        if (!pill) {
          var you = document.createElement("span");
          you.className = "pill connected";
          var dot = document.createElement("span");
          dot.className = "dot";
          you.appendChild(dot);
          you.appendChild(document.createTextNode("you"));
          el.appendChild(you);
        }
      } else {
        if (pill) pill.remove();
        if (button) {
          if (m.paired) {
            button.remove();
          } else {
            button.setAttribute("data-pair-with", m.id);
          }
        }
        el.style.opacity = "";
      }
      wrap.appendChild(el);
    });
  }

  function renderVerify(s) {
    var block = states["verify — the moment that matters"];
    if (!block || !s.pairing) return;
    var groups = block.querySelectorAll(".code .g");
    var code = s.pairing.code || "";
    if (groups.length === 2 && code.length === 6) {
      groups[0].textContent = digits(code.slice(0, 3));
      groups[1].textContent = digits(code.slice(3));
    }
    var note = block.querySelector(".code-note");
    if (note) note.textContent = "also showing on " + s.pairing.with + " right now";
    var chips = block.querySelectorAll(".both .chip");
    if (chips.length === 2) {
      chips[0].textContent = s.name || "this machine";
      chips[1].textContent = s.pairing.with;
    }
  }

  function renderOutcome(s, ok) {
    var block = states[ok ? "paired" : "numbers differ"];
    if (!block || !s.pairing) return;
    if (ok) {
      var bold = block.querySelector(".outcome b");
      if (bold) bold.textContent = s.pairing.with + " is trusted.";
    }
  }

  window.seam.onState(function (s) {
    var pairing = s.pairing;
    if (pairing && pairing.state === "showing") {
      renderVerify(s);
      show(["verify — the moment that matters"]);
    } else if (pairing && pairing.state === "paired") {
      renderOutcome(s, true);
      show(["paired"]);
    } else if (pairing && pairing.state === "declined") {
      renderOutcome(s, false);
      show(["numbers differ"]);
    } else if ((s.discovered || []).length || (s.peers || []).length) {
      renderFound(s);
      show(["machines found"]);
    } else {
      show(["discovering"]);
    }
  });

  document.addEventListener("click", function (event) {
    var closest = event.target.closest ? event.target.closest.bind(event.target) : function () { return null; };
    var pair = closest("[data-pair-with]");
    if (pair) {
      window.seam.post("/action/pair/" + pair.getAttribute("data-pair-with"));
      return;
    }
    var button = closest(".dialog .actions .btn");
    if (!button) return;
    if (button.classList.contains("primary")) {
      window.seam.post("/action/pair/confirm");
    } else {
      window.seam.post("/action/pair/decline");
    }
  });
})();
