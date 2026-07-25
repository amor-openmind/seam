// Add a machine — both commands, built from what the daemon actually knows.
//
// No OS switch: this page is read on one machine and the command is run on a different
// one, so defaulting to the reader's browser is wrong by construction — it handed a shell
// command to someone standing at a PowerShell prompt. Showing both, labelled, removes the
// state and the mistake together.
//
// The address is the daemon's LAN address, not the browser's view of itself: a page served
// on loopback only ever sees 127.0.0.1, which is meaningless on the machine being added.
// The script comes from the public releases page because seam's own page listens on
// loopback and could never be reached from there.
(function () {
  "use strict";
  var RELEASES = "https://github.com/amor-openmind/seam-releases/releases/latest/download";
  var server = null;

  function commands() {
    return {
      win: '$env:SEAM_SERVER="' + server + '"; iwr -useb ' + RELEASES + '/join.ps1 | iex',
      unix: 'SEAM_SERVER=' + server + ' sh -c "$(curl -fsSL ' + RELEASES + '/join.sh)"'
    };
  }

  function render() {
    var win = document.querySelector("[data-join-win]");
    var unix = document.querySelector("[data-join-unix]");
    if (!server) {
      if (win) win.textContent = "—";
      if (unix) unix.textContent = "—";
      return;
    }
    var c = commands();
    if (win) win.textContent = c.win;
    if (unix) unix.textContent = c.unix;
  }

  window.seam.onState(function (s) {
    server = s.lan ? s.lan + ":" + (s.seamPort || 24810) : null;
    var warn = document.querySelector("[data-join-nonet]");
    if (warn) warn.style.display = s.lan ? "none" : "";
    render();
  });

  document.addEventListener("click", function (event) {
    var closest = event.target.closest ? event.target.closest.bind(event.target) : function () { return null; };
    var btn = closest("[data-copy]");
    if (!btn || !server) return;
    var text = commands()[btn.getAttribute("data-copy")];
    var done = function () { btn.textContent = "Copied"; setTimeout(function () { btn.textContent = "Copy"; }, 1400); };
    if (navigator.clipboard) navigator.clipboard.writeText(text).then(done, done);
    else done();
  });

  render();
})();
