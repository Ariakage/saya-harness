(function () {
  try {
    var root = document.documentElement;
    var paramTheme = null;
    try {
      paramTheme = new URLSearchParams(window.location.search).get("theme");
    } catch (_) {}
    var cached = null;
    try {
      cached = window.localStorage.getItem("soyo.theme");
    } catch (_) {}
    var resolved =
      paramTheme === "light" || paramTheme === "dark"
        ? paramTheme
        : cached === "light" || cached === "dark"
          ? cached
          : window.matchMedia &&
              window.matchMedia("(prefers-color-scheme: light)").matches
            ? "light"
            : "dark";
    root.dataset.theme = resolved;
    root.style.colorScheme = resolved;
    if (!root.dataset.convFont) root.dataset.convFont = "default";
    if (!root.dataset.convSize) root.dataset.convSize = "14";
    if (!root.dataset.reduceMotion) root.dataset.reduceMotion = "false";
  } catch (_) {}
})();
