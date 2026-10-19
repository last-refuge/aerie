# Aerie

Google Drive for Linux. Your files, everywhere.

Aerie is a native Linux Google Drive app built with Rust, GTK4 and libadwaita. All Drive access runs on your computer and talks directly to Google; there is no Aerie-hosted backend.

The project is in early development. See the [implementation plan](llm-docs/implementation-plan.md) and [brand guidelines](llm-docs/brand-guidelines.md).

## Building

Requires Rust 1.92+, GTK 4.16+ and libadwaita 1.6+. On Arch Linux:

```sh
sudo pacman -S --needed gtk4 libadwaita rustup
cargo run -p aerie-app
```

## Drive sign-in spike

Create a Desktop app OAuth client in a Google Cloud test project, save its JSON to `~/.config/aerie/oauth-client.json`, then run:

```sh
cargo run -p aerie-drive --example drive_spike
```

It signs in with read-only Drive access, keeps the sign-in in the system keyring, and lists My Drive. Add `-- --sign-out` to remove the stored sign-in.

## Layout

| Crate | Responsibility |
| --- | --- |
| `aerie-app` | GTK application, views and components |
| `aerie-core` | Shared domain models and commands |
| `aerie-drive` | OAuth and Drive API adapter |
| `aerie-sync` | Reconciliation and transfer scheduling |
| `aerie-store` | SQLite schema and migrations |

## License

[MIT](LICENSE)
