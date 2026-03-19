//! Shared path normalization and resolution utilities.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

/// Collapse `.` and `..` components, returning a canonical absolute path.
pub fn normalize(path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for seg in path.split('/') {
        match seg {
            "" | "." => {}
            ".." => { parts.pop(); }
            s    => parts.push(s),
        }
    }
    if parts.is_empty() {
        String::from("/")
    } else {
        let mut out = String::new();
        for p in &parts {
            out.push('/');
            out.push_str(p);
        }
        out
    }
}

/// Resolve `path` against `cwd`: absolute paths pass through; relative paths
/// are joined to `cwd`.  The result is then normalised (`.` and `..` removed).
pub fn resolve(cwd: &str, path: &str) -> String {
    let raw = if path.starts_with('/') {
        path.to_string()
    } else if path.is_empty() {
        cwd.to_string()
    } else {
        let mut base = cwd.to_string();
        if !base.ends_with('/') { base.push('/'); }
        base.push_str(path);
        base
    };
    normalize(&raw)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{serial_print, serial_println};

    // ── normalize ────────────────────────────────────────────────────────

    #[test_case]
    fn test_normalize_root() {
        serial_print!("test_normalize_root... ");
        assert_eq!(normalize("/"), "/");
        serial_println!("[ok]");
    }

    #[test_case]
    fn test_normalize_simple() {
        serial_print!("test_normalize_simple... ");
        assert_eq!(normalize("/foo/bar"), "/foo/bar");
        serial_println!("[ok]");
    }

    #[test_case]
    fn test_normalize_trailing_slash() {
        serial_print!("test_normalize_trailing_slash... ");
        assert_eq!(normalize("/foo/bar/"), "/foo/bar");
        serial_println!("[ok]");
    }

    #[test_case]
    fn test_normalize_double_slash() {
        serial_print!("test_normalize_double_slash... ");
        assert_eq!(normalize("/foo//bar"), "/foo/bar");
        serial_println!("[ok]");
    }

    #[test_case]
    fn test_normalize_dot() {
        serial_print!("test_normalize_dot... ");
        assert_eq!(normalize("/foo/./bar"), "/foo/bar");
        serial_println!("[ok]");
    }

    #[test_case]
    fn test_normalize_dotdot() {
        serial_print!("test_normalize_dotdot... ");
        assert_eq!(normalize("/foo/bar/../baz"), "/foo/baz");
        serial_println!("[ok]");
    }

    #[test_case]
    fn test_normalize_dotdot_past_root() {
        serial_print!("test_normalize_dotdot_past_root... ");
        assert_eq!(normalize("/foo/../../bar"), "/bar");
        serial_println!("[ok]");
    }

    #[test_case]
    fn test_normalize_all_dotdot() {
        serial_print!("test_normalize_all_dotdot... ");
        assert_eq!(normalize("/foo/bar/../.."), "/");
        serial_println!("[ok]");
    }

    // ── resolve ──────────────────────────────────────────────────────────

    #[test_case]
    fn test_resolve_absolute_ignores_cwd() {
        serial_print!("test_resolve_absolute_ignores_cwd... ");
        assert_eq!(resolve("/home", "/etc/passwd"), "/etc/passwd");
        serial_println!("[ok]");
    }

    #[test_case]
    fn test_resolve_relative() {
        serial_print!("test_resolve_relative... ");
        assert_eq!(resolve("/foo", "bar"), "/foo/bar");
        serial_println!("[ok]");
    }

    #[test_case]
    fn test_resolve_relative_with_dotdot() {
        serial_print!("test_resolve_relative_with_dotdot... ");
        assert_eq!(resolve("/foo/bar", "../baz"), "/foo/baz");
        serial_println!("[ok]");
    }

    #[test_case]
    fn test_resolve_empty_returns_cwd() {
        serial_print!("test_resolve_empty_returns_cwd... ");
        assert_eq!(resolve("/foo/bar", ""), "/foo/bar");
        serial_println!("[ok]");
    }

    #[test_case]
    fn test_resolve_from_root() {
        serial_print!("test_resolve_from_root... ");
        assert_eq!(resolve("/", "foo"), "/foo");
        serial_println!("[ok]");
    }
}
