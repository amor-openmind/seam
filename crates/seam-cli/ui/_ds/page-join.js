// Add a machine — the command is built from this machine's real address and version.
(function () {
  "use strict";
  var os = "unix";
  var state = null;

  function host() {
    // The address another machine would use: this page's host, with seam's port. The
    // browser knows the former; the daemon reports the latter.
    var name = location.hostname === "127.0.0.1" || location.hostname === "localhost"
      ? "THIS-MACHINE"
      : location.hostname;
    return name + ":" + (state && state.seamPort ? state.seamPort : "24810");
  }

  function uiHost() { return location.host; }

  function render() {
    var el = document.querySelector("[data-join-cmd]");
    if (!el) return;
    el.textContent = os === "windows"
      ? '$env:SEAM_SERVER="' + host() + '"; iwr -useb http://' + uiHost() + '/join.ps1 | iex'
      : 'SEAM_SERVER=' + host() + ' sh -c "$(curl -fsSL http://' + uiHost() + '/join.sh)"';
  }

  window.seam.onState(function (s) {
    state = { seamPort: s.seamPort || s.port };
    render();
  });

  document.addEventListener("click", function (event) {
    var closest = event.target.closest ? event.target.closest.bind(event.target) : function () { return null; };
    var pick = closest("[data-os]");
    if (pick) {
      os = pick.getAttribute("data-os");
      var all = document.querySelectorAll("[data-os]");
      for (var i = 0; i < all.length; i++) all[i].setAttribute("aria-pressed", all[i] === pick ? "true" : "false");
      render();
      return;
    }
    if (closest("[data-copy]")) {
      var cmd = document.querySelector("[data-join-cmd]");
      var btn = closest("[data-copy]");
      if (!cmd) return;
      var done = function () { btn.textContent = "Copied"; setTimeout(function () { btn.textContent = "Copy"; }, 1400); };
      if (navigator.clipboard) navigator.clipboard.writeText(cmd.textContent).then(done, done);
      else done();
    }
  });

  render();
})();
