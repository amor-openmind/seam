// seam — bindings shared by every page.
//
// The pages carry ONE example of each repeating element, which the design owns and this
// script clones and fills. Nothing here invents structure, styling or copy — and nothing
// here leaves example content on screen: a page that shows a designer's sample machine
// name or a made-up timestamp is lying to the person reading it.
(function () {
  "use strict";
  var POLL_MS = 900;
  var listeners = [];

  window.seam = {
    short: function (id) { return String(id || "").slice(0, 8); },
    // Capture a repeating element before any data arrives, so every render clones the
    // authored markup rather than the previous render's leftovers.
    template: function (root, selector) {
      if (!root) return null;
      var first = root.querySelector(selector);
      return first ? first.cloneNode(true) : null;
    },
    clear: function (root, selector) {
      if (!root) return;
      var all = root.querySelectorAll(selector);
      for (var i = 0; i < all.length; i++) all[i].remove();
    },
    text: function (root, selector, value) {
      var el = root && root.querySelector(selector);
      if (el) el.textContent = value == null ? "" : String(value);
    },
    show: function (selector, on) {
      var el = document.querySelector(selector);
      if (el) el.style.display = on ? "" : "none";
    },
    onState: function (fn) { listeners.push(fn); },
    post: function (path) { return fetch(path, { method: "POST" }).then(tick).catch(function () {}); }
  };

  var missed = 0;
  function tick() {
    return fetch("/state")
      .then(function (r) { return r.json(); })
      .then(function (s) {
        missed = 0;
        listeners.forEach(function (fn) { try { fn(s); } catch (e) {} });
      })
      .catch(function () {
        // A few misses is a hiccup; a run of them means this daemon is gone — replaced by
        // a newer seam, or stopped. Saying so beats polling a port that may now belong to
        // something else.
        if (++missed === 4) window.dispatchEvent(new Event("seam:replaced"));
      });
  }

  // Header and navigation exist on every page.
  window.seam.onState(function (s) {
    var mach = document.querySelector("header.top .machine");
    if (mach) mach.innerHTML = s.name + " · " + window.seam.short(s.id) + " · <b>" + (s.role || "") + "</b><br>v" + s.version + " · " + s.platform;
    var here = location.pathname === "/" ? "/" : location.pathname;
    var tabs = document.querySelectorAll("nav.tabs a");
    for (var i = 0; i < tabs.length; i++) {
      var href = tabs[i].getAttribute("href");
      if (href === here) tabs[i].setAttribute("aria-current", "page");
      else tabs[i].removeAttribute("aria-current");
    }
  });

  tick();
  setInterval(tick, POLL_MS);
})();
