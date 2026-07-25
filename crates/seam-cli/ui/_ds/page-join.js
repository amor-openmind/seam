// Add a machine — one line, and it never goes stale.
//
// No OS switch: this page is read on one machine and the command is run on a different
// one, so defaulting to the reader's browser is wrong by construction.
//
// No version either. It was carried in the command and in an environment variable to keep
// a fleet in step, which turned a one-line join into two things to keep matched by hand.
// GitHub resolves `latest` itself, so the command is fixed text plus this machine's
// address — the same line works next month.
//
// The address is the daemon's LAN address, not the browser's view of itself: a page served
// on loopback only ever sees 127.0.0.1, meaningless on the machine being added.
(function () {
  "use strict";
  var LATEST = "https://github.com/amor-openmind/seam-releases/releases/latest/download";
  var server = null;

  function commands() {
    return {
      win: '$env:SEAM_SERVER="' + server + '"; iwr -useb ' + LATEST + '/join.ps1 | iex',
      unix: 'SEAM_SERVER=' + server + ' sh -c "$(curl -fsSL ' + LATEST + '/join.sh)"'
    };
  }

  function render() {
    var win = document.querySelector("[data-join-win]");
    var unix = document.querySelector("[data-join-unix]");
    if (win) win.textContent = server ? commands().win : "—";
    if (unix) unix.textContent = server ? commands().unix : "—";
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
