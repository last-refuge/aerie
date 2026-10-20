//! Redaction for diagnostics. Default logs may name operations and counts, never file
//! names, paths, account addresses, tokens or content.

use std::path::Path;

/// Keeps only the extension, which helps diagnose type-specific problems.
pub fn path(path: &Path) -> String {
    match path.extension().and_then(|ext| ext.to_str()) {
        Some(ext) => format!("<path>.{ext}"),
        None => "<path>".to_owned(),
    }
}

/// Keeps the first character and the domain: `j…@example.com`.
pub fn email(address: &str) -> String {
    match address.split_once('@') {
        Some((local, domain)) => {
            let first = local.chars().next().map(String::from).unwrap_or_default();
            format!("{first}…@{domain}")
        }
        None => "<email>".to_owned(),
    }
}

/// Drive IDs are opaque but identifying; keep a short prefix to correlate log lines.
pub fn id(id: &str) -> String {
    let prefix: String = id.chars().take(4).collect();
    format!("{prefix}…")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_identifying_details() {
        assert_eq!(path(Path::new("/home/jason/Taxes 2025.pdf")), "<path>.pdf");
        assert_eq!(path(Path::new("/home/jason/notes")), "<path>");
        assert_eq!(email("jason@example.com"), "j…@example.com");
        assert_eq!(email("not-an-address"), "<email>");
        assert_eq!(id("1AbCdEfGh"), "1AbC…");
    }
}
