// Transfers — shows what is actually moving, and says so when nothing is.
(function () {
  "use strict";
  window.seam.onState(function (s) {
    var now = document.querySelector("[data-xfer]");
    if (now) {
      now.style.display = s.transfer ? "" : "none";
      if (s.transfer) {
        window.seam.text(now, ".t", s.transfer.what);
        window.seam.text(now, ".s", s.transfer.detail);
      }
    }
    window.seam.show("[data-xfer-empty]", !s.transfer);

    var shares = [];
    if (s.shares) {
      if (s.shares.text) shares.push("text");
      if (s.shares.images) shares.push("images");
      if (s.shares.files) shares.push("files & folders");
    }
    window.seam.text(document, "[data-sharing]", shares.length ? shares.join(" · ") : "nothing — all sharing is switched off");
  });
})();
