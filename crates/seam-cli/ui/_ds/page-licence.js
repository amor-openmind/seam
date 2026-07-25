// Licence — activation, bound to the daemon's real answer.
(function () {
  "use strict";

  function setState(which) {
    var ok = document.querySelector("[data-licence-ok]");
    var bad = document.querySelector("[data-licence-bad]");
    if (ok) ok.style.display = which === "ok" ? "" : "none";
    if (bad) bad.style.display = which === "bad" ? "" : "none";
  }
  setState(null);

  window.seam.onState(function (s) {
    if (!s.licence) { setState(null); return; }
    setState("ok");
    window.seam.text(document, "[data-licence-name]", s.licence.name);
    window.seam.text(document, "[data-licence-expiry]", s.licence.expires ? " · until day " + s.licence.expires : " · no expiry");
  });

  document.addEventListener("click", function (event) {
    var closest = event.target.closest ? event.target.closest.bind(event.target) : function () { return null; };
    if (!closest("[data-activate]")) return;
    var input = document.querySelector("[data-licence-input]");
    if (!input || !input.value.trim()) return;
    fetch("/action/activate", { method: "POST", body: input.value.trim() })
      .then(function (r) { return r.json(); })
      .then(function (res) {
        if (res.ok) {
          setState("ok");
          window.seam.text(document, "[data-licence-name]", res.name || "");
        } else {
          setState("bad");
          window.seam.text(document, "[data-licence-error]", res.error || "That licence was not accepted.");
        }
      })
      .catch(function () {
        setState("bad");
        window.seam.text(document, "[data-licence-error]", "seam could not be reached on this machine.");
      });
  });
})();
