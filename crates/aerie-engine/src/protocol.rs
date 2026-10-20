//! The local protocol between the app and `aerie-service`: newline-delimited JSON over a
//! Unix socket in the user's runtime directory. Each side first sends a [`Hello`]; a
//! protocol mismatch means one side was upgraded, and the app restarts the service.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Bump when [`crate::Command`] or [`crate::Event`] change incompatibly.
pub const PROTOCOL: u32 = 2;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hello {
    pub protocol: u32,
    pub version: String,
}

impl Hello {
    pub fn current() -> Self {
        Self {
            protocol: PROTOCOL,
            version: env!("CARGO_PKG_VERSION").to_owned(),
        }
    }
}

/// `$XDG_RUNTIME_DIR/aerie/service.sock`. The runtime directory belongs to the user alone,
/// and the service also checks each peer's user ID.
pub fn socket_path() -> PathBuf {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join(format!("aerie-{}", user_id())));
    // Development builds use their own socket, so they never talk to an installed service.
    let name = if cfg!(debug_assertions) {
        "service-devel.sock"
    } else {
        "service.sock"
    };
    runtime.join("aerie").join(name)
}

fn user_id() -> String {
    std::env::var("USER").unwrap_or_else(|_| "user".to_owned())
}
