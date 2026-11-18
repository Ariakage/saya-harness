import { useSyncExternalStore } from "react";

/* Theme preference: system | light | dark (mirrors desktop_prefs.theme).
   The effective theme is cached in localStorage["soyo.theme"] for first paint. */

export type ThemePref = "system" | "light" | "dark";
export type EffectiveTheme = "light" | "dark";

const PREF_KEY = "soyo.theme.pref";
const EFFECTIVE_KEY = "soyo.theme";

let pref: ThemePref = "system";
const listeners = new Set<() => void>();

function systemTheme(): EffectiveTheme {
  return window.matchMedia("(prefers-color-scheme: light)").matches
    ? "light"
    : "dark";
}

function effective(): EffectiveTheme {
  return pref === "system" ? systemTheme() : pref;
}

function applyEffective() {
  const t = effective();
  document.documentElement.dataset.theme = t;
  document.documentElement.style.colorScheme = t;
  try {
    localStorage.setItem(EFFECTIVE_KEY, t);
  } catch {
    /* ignore */
  }
}

function notify() {
  listeners.forEach((l) => l());
}

export function initTheme() {
  try {
    const saved = localStorage.getItem(PREF_KEY);
    if (saved === "light" || saved === "dark" || saved === "system") {
      pref = saved;
    }
  } catch {
    /* ignore */
  }
  applyEffective();
  window
    .matchMedia("(prefers-color-scheme: light)")
    .addEventListener("change", () => {
      if (pref === "system") {
        applyEffective();
        notify();
      }
    });
}

export function setThemePref(next: ThemePref) {
  pref = next;
  try {
    localStorage.setItem(PREF_KEY, next);
  } catch {
    /* ignore */
  }
  applyEffective();
  notify();
}

export function cycleThemePref(): ThemePref {
  const next: ThemePref =
    pref === "system" ? "light" : pref === "light" ? "dark" : "system";
  setThemePref(next);
  return next;
}

export function useThemePref(): ThemePref {
  return useSyncExternalStore(
    (cb) => {
      listeners.add(cb);
      return () => listeners.delete(cb);
    },
    () => pref,
  );
}

export function useEffectiveTheme(): EffectiveTheme {
  return useSyncExternalStore(
    (cb) => {
      listeners.add(cb);
      return () => listeners.delete(cb);
    },
    effective,
  );
}
