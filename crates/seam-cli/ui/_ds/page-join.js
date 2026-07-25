// Add a machine — the command, built from what the daemon actually knows.
//
// Two things this gets right that the first version did not: the address is this
// machine's LAN address as reported by the daemon (the browser only ever sees
// 127.0.0.1, which is meaningless on the machine being added), and the script is fetched
// from the public releases page rather than from here — seam's page listens on loopback
// only, so the other machine could never have reached it.
(function () {
  "use strict";
  var RELEASES = "https://github.com/amor-openmind/seam-releases/releases/latest/download";
  var os = navigator.userAgent.indexOf("Windows") !== -1 ? "windows" : "unix";
  var server = null;

  function render() {
    var el = document.querySelector("[data-join-cmd]");
    if (!el) return;
    if (!server) {
      el.textContent = "Waiting for this machine's network address…";
      return;
    }
    el.textContent = os === "windows"
      ? '$env:SEAM_SERVER="' + server + '"; iwr -useb ' + RELEASES + '/join.ps1 | iex'
      : 'SEAM_SERVER=' + server + ' sh -c "$(curl -fsSL ' + RELEASES + '/join.sh)"';
  }

  window.seam.onState(function (s) {
    // No LAN address means this machine is not on a network another can reach, and the
    // command would be a lie. Say that instead of printing something that cannot work.
    server = s.lan ? s.lan + ":" + (s.seamPort || 24810) : null;
    var warn = document.querySelector("[data-join-nonet]");
    if (warn) warn.style.display = s.lan ? "none" : "";
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
      if (!cmd || !server) return;
      var done = function () { btn.textContent = "Copied"; setTimeout(function () { btn.textContent = "Copy"; }, 1400); };
      if (navigator.clipboard) navigator.clipboard.writeText(cmd.textContent).then(done, done);
      else done();
    }
  });

  // Match the switch to the machine most likely being added — the one you are reading on.
  var initial = document.querySelector('[data-os="' + os + '"]');
  if (initial) {
    var all = document.querySelectorAll("[data-os]");
    for (var i = 0; i < all.length; i++) all[i].setAttribute("aria-pressed", all[i] === initial ? "true" : "false");
  }
  render();
})();
