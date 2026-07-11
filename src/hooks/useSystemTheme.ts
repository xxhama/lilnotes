import { useEffect } from "react";

/**
 * Mirrors the macOS system appearance (light/dark) onto the app by toggling
 * the `.dark` class on <html>. The dark-mode CSS tokens in index.css activate
 * under `.dark`. Keeps in sync when the user changes system appearance while
 * the app is open.
 */
export function useSystemTheme() {
  useEffect(() => {
    const media = window.matchMedia("(prefers-color-scheme: dark)");
    const apply = (isDark: boolean) => {
      document.documentElement.classList.toggle("dark", isDark);
    };

    apply(media.matches);

    const onChange = (e: MediaQueryListEvent) => apply(e.matches);
    media.addEventListener("change", onChange);
    return () => media.removeEventListener("change", onChange);
  }, []);
}
