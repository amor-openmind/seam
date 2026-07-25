// Add a machine — both commands, built from what the daemon actually knows.
//
// No OS switch: this page is read on one machine and the command is run on a different
// one, so defaulting to the reader's browser is wrong by construction — it handed a shell
// command to someone standing at a PowerShell prompt.
//
// The version is written INTO the command rather than fetched from this machine. The
// original design had the new machine ask this one over HTTP, which could never work:
// seam's page listens on loopback, and the port peers use is QUIC over UDP, so there is
// nothing on the network to ask. The page already knows the version; carrying it costs
// nothing and removes a network round trip that was doomed.
//
// The address is the daemon's LAN address, not the browser's view of itself: a page served
// on loopback only ever sees 127.0.0.1, meaningless on the machine being added.
(function () {
  "use strict";
  var RELEASES = "https://github.com/amor-openmind/seam-releases/releases";
  var server = null;
  var version = null;

  function commands() {
    var base = RELEASES + "/download/v" + version;
    return {
      win: '$env:SEAM_SERVER="' + server + '"; $env:SEAM_VERSION="' + version + '"; iwr -useb ' + base + '/join.ps1 | iex',
      unix: 'SEAM_SERVER=' + server + ' SEAM_VERSION=' + version + ' sh -c "$(curl -fsSL ' + base + '/join.sh)"'
    };
  }

  function render() {
    var win = document.querySelector("[data-join-win]");
    var unix = document.querySelector("[data-join-unix]");
    var ready = server && version;
    if (win) win.textContent = ready ? commands().win : "—";
    if (unix) unix.textContent = ready ? commands().unix : "—";
  }

  window.seam.onState(function (s) {
    server = s.lan ? s.lan + ":" + (s.seamPort || 24810) : null;
    version = s.version || null;
    var warn = document.querySelector("[data-join-nonet]");
    if (warn) warn.style.display = s.lan ? "none" : "";
    render();
  });

  document.addEventListener("click", function (event) {
    var closest = event.target.closest ? event.target.closest.bind(event.target) : function () { return null; };
    var btn = closest("[data-copy]");
    if (!btn || !server || !version) return;
    var text = commands()[btn.getAttribute("data-copy")];
    var done = function () { btn.textContent = "Copied"; setTimeout(function () { btn.textContent = "Copy"; }, 1400); };
    if (navigator.clipboard) navigator.clipboard.writeText(text).then(done, done);
    else done();
  });

  render();
})();
