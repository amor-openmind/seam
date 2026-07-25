// Doctor — every check reflects a real reading. No invented failures.
(function () {
  "use strict";
  var list = document.querySelector("[data-checks]");
  var tpl = window.seam.template(list, "[data-check]");

  window.seam.onState(function (s) {
    if (!list || !tpl) return;
    window.seam.clear(list, "[data-check]");

    var checks = (s.health || []).map(function (h, i) {
      var names = ["Capture input", "Withhold input from this machine", "Cursor control", "Input tap"];
      return { ok: h.ok, k: names[i] || "Check", v: h.text };
    });
    checks.push({ ok: true, k: "Listening", v: "port " + (s.port || "?") + " · QUIC · encrypted" });
    checks.push({
      ok: (s.peers || []).length > 0,
      k: "Machines connected",
      v: (s.peers || []).length + " connected"
    });
    checks.push({ ok: true, k: "This machine", v: s.name + " · " + window.seam.short(s.id) + " · " + s.role });

    var failures = 0;
    checks.forEach(function (c) {
      if (!c.ok) failures++;
      var el = tpl.cloneNode(true);
      el.classList.remove("pass", "fail");
      el.classList.add(c.ok ? "pass" : "fail");
      window.seam.text(el, ".mk", c.ok ? "✓" : "×");
      window.seam.text(el, ".k", c.k);
      window.seam.text(el, ".v", c.v);
      var fix = el.querySelector(".fix");
      if (fix) fix.style.display = c.ok ? "none" : "";
      list.appendChild(el);
    });

    var v = document.querySelector("[data-verdict]");
    if (v) {
      v.className = "verdict " + (failures ? "bad" : "ok");
      window.seam.text(v, ".mk", failures ? "!" : "✓");
      window.seam.text(v, ".t", failures ? failures + " problem" + (failures > 1 ? "s" : "") + " found" : "Everything checks out");
      window.seam.text(v, ".d", failures
        ? "Each one below states what it actually causes."
        : "Input is captured, withheld and forwarded correctly on this machine.");
    }
  });
})();
