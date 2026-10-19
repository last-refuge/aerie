# 0001: GTK shell spike findings

**Status:** Phase 0 evidence, October 19, 2026. Supports the plan's default of Rust + GTK4/libadwaita.

## What the spike shows

A libadwaita window with a collapsible sidebar, a virtualized and sortable `ColumnView` file list, breadcrumbs, filename search, an empty state and an alert dialog, all over sample data. Navigation and search logic live in `aether-core` (`BrowseState`) with unit tests; the GTK layer only renders it.

## Findings

- **Baseline:** GTK 4.16 and libadwaita 1.6 are the minimums, for CSS custom properties. Arch ships newer releases.
- **Brand tokens map onto libadwaita variables.** Overriding `--window-bg-color`, `--sidebar-bg-color`, `--accent-bg-color` and related variables restyles stock widgets without per-widget overrides.
- **Dark and light need separate providers.** `@media (prefers-color-scheme: dark)` in an app-added `GtkCssProvider` did not follow libadwaita's dark style, so `style-dark.css` and `style-hc.css` are layered on and off from `AdwStyleManager` notifications.
- **Accent overrides need the named colors too.** Some libadwaita accent variables derive from `@accent_bg_color` and `@accent_fg_color`, so both are set. Label contrast on the suggested dialog button in dark mode still needs a check on a real desktop.
- **The primary gradient uses darker stops** (`#0369a1 → #4338ca → #6d28d9 → #0e7490`) so white label text clears 4.5:1 at every stop. The bright brand gradient stays reserved for the logo.
- **Light palette (new decision, to validate):** window `#f8fafc`, sidebar `#eef2f7`, cards `#ffffff`, text `#0b1220`, secondary text `#475569`, accent `#0369a1`.

## Open items

- Inter and JetBrains Mono are requested by name but not bundled; the app falls back to the system font when they're missing. Bundling (both OFL) is a packaging decision.
- Icons are GTK symbolic icons for now. Lucide (ISC) assets are still to be added.
- Keyboard, screen reader, fractional scaling and high-contrast checks on GNOME, KDE and Hyprland are still to do.
- Headless screenshots work with `gtk4-broadwayd`, but its input handling is too unreliable for automated UI tests.
