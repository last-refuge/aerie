# Aether

Google Drive for Linux. Your files, everywhere.

Aether is a native Linux Google Drive app built with Rust, GTK4 and libadwaita. All Drive access runs on your computer and talks directly to Google; there is no Aether-hosted backend.

The project is in early development. See the [implementation plan](llm-docs/implementation-plan.md) and [brand guidelines](llm-docs/brand-guidelines.md).

## Building

Requires Rust 1.85+, GTK 4.14+ and libadwaita 1.5+. On Arch Linux:

```sh
sudo pacman -S --needed gtk4 libadwaita rustup
cargo run -p aether-app
```

## Layout

| Crate | Responsibility |
| --- | --- |
| `aether-app` | GTK application, views and components |
| `aether-core` | Shared domain models and commands |
| `aether-drive` | OAuth and Drive API adapter |
| `aether-sync` | Reconciliation and transfer scheduling |
| `aether-store` | SQLite schema and migrations |
