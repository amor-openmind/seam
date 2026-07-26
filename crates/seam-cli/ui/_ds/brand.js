// seam — the wordmark lockup, defined once and applied to every page.
//
// The mark comes from the Design System (assets/logo): two indigo bracket forms meeting
// at a join, bridged by three thread-orange stitches. "The join is the mark" — so the
// symbol carries the product's whole idea and the pages should show it rather than a full
// stop standing in for it.
//
// Defined here rather than pasted into ten page headers: a lockup copied ten times is a
// lockup that will differ in ten ways by next month. Each page keeps its own <h1>; this
// replaces its contents with the authored lockup and sets the favicon to the same mark.
(function () {
  "use strict";

  var MARK = "_ds/logo/seam-logo-128.png";
  var ICON = "_ds/logo/seam-logo-32.png";

  var CSS = ".seam-lockup{display:inline-flex;align-items:center;gap:11px}" +
    ".seam-lockup img{width:1.05em;height:1.05em;object-fit:contain;flex:none;" +
    "transform:translateY(.02em)}" +
    ".seam-lockup .word{font:inherit;letter-spacing:-.02em}" +
    // The stitch keeps its accent colour: it is the same idea as the orange in the mark,
    // and dropping it would leave the wordmark reading as plain text.
    ".seam-lockup .stitch{color:var(--text-accent)}";

  function apply() {
    var h1 = document.querySelector("header.top h1, .shell > h1");
    if (!h1 || h1.querySelector(".seam-lockup")) return;

    if (!document.querySelector("style[data-seam-brand]")) {
      var style = document.createElement("style");
      style.setAttribute("data-seam-brand", "");
      style.textContent = CSS;
      document.head.appendChild(style);
    }

    // Anything after the wordmark in the heading — a tagline, for instance — is kept.
    var tail = "";
    var extra = h1.querySelector("span:not(.stitch)");
    if (extra) tail = extra.outerHTML;

    h1.innerHTML =
      '<span class="seam-lockup">' +
      '<img src="' + MARK + '" alt="">' +
      '<span class="word">seam<span class="stitch">.</span></span>' +
      "</span>" + (tail ? " " + tail : "");

    var link = document.querySelector('link[rel="icon"]');
    if (!link) {
      link = document.createElement("link");
      link.setAttribute("rel", "icon");
      document.head.appendChild(link);
    }
    link.setAttribute("type", "image/png");
    link.setAttribute("href", ICON);
  }

  if (document.readyState === "loading") document.addEventListener("DOMContentLoaded", apply);
  else apply();
})();
