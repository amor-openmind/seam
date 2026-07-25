// Settings — bound to the values the daemon actually persists.
(function () {
  "use strict";
  window.seam.onState(function (s) {
    window.seam.text(document, "[data-self-name]", s.name);
    window.seam.text(document, "[data-self-id]", s.id);
    window.seam.text(document, "[data-self-version]", "v" + s.version + " · " + s.platform);
    window.seam.text(document, "[data-self-role]", s.role);

    var toggles = [["text", s.shares && s.shares.text], ["images", s.shares && s.shares.images], ["files", s.shares && s.shares.files]];
    toggles.forEach(function (pair) {
      var el = document.querySelector('[data-share="' + pair[0] + '"]');
      if (!el) return;
      el.classList.toggle("on", !!pair[1]);
      el.setAttribute("aria-checked", pair[1] ? "true" : "false");
    });
    var su = document.querySelector("[data-startup]");
    if (su) { su.classList.toggle("on", !!s.startup); su.setAttribute("aria-checked", s.startup ? "true" : "false"); }
  });

  document.addEventListener("click", function (event) {
    var closest = event.target.closest ? event.target.closest.bind(event.target) : function () { return null; };
    var share = closest("[data-share]");
    if (share) {
      var on = share.getAttribute("aria-checked") !== "true";
      window.seam.post("/action/share/" + share.getAttribute("data-share") + "/" + (on ? "on" : "off"));
      return;
    }
    var su = closest("[data-startup]");
    if (su) {
      var want = su.getAttribute("aria-checked") !== "true";
      window.seam.post("/action/startup/" + (want ? "on" : "off"));
    }
  });
})();
