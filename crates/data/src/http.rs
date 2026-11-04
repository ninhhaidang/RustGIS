//! The certificate trust of RustGIS's native HTTPS clients (desktop and command line).
//!
//! Clients verify servers with the operating system's trust (Windows and macOS: their own
//! verifiers, which honour what the system distrusts; Linux and the BSDs: the system bundle, or
//! the `SSL_CERT_FILE` and `SSL_CERT_DIR` the platform verifier reads). The browser uses its own.

use std::sync::LazyLock;

use ureq::tls::{RootCerts, TlsConfig};

/// The TLS settings every native `ureq` agent takes: platform verification, except on a Linux or
/// BSD system with no CA certificates at all (a slim container; a store it can't read still
/// counts) and no `SSL_CERT_FILE` or `SSL_CERT_DIR`, where the bundled Mozilla roots keep public
/// HTTPS working. An explicitly chosen trust source is never widened with them.
///
/// The platform verifier reads the system's certificates each time an agent sets up TLS, so an
/// agent should be made once and kept, its requests leaving its TLS settings alone.
pub fn tls_config() -> TlsConfig {
    static ROOTS: LazyLock<RootCerts> = LazyLock::new(system_roots);
    TlsConfig::builder().root_certs(ROOTS.clone()).build()
}

#[cfg(all(unix, not(target_vendor = "apple"), not(target_os = "android")))]
fn system_roots() -> RootCerts {
    roots(std::env::var_os("SSL_CERT_FILE"), std::env::var_os("SSL_CERT_DIR"), || {
        // Only paths that exist are probed, so an error is a store that failed to load.
        let loaded = rustls_native_certs::load_native_certs();
        loaded.certs.is_empty() && loaded.errors.is_empty()
    })
}

#[cfg(not(all(unix, not(target_vendor = "apple"), not(target_os = "android"))))]
fn system_roots() -> RootCerts {
    RootCerts::PlatformVerifier
}

/// The roots on Linux and the BSDs: the bundled ones only when neither `SSL_CERT_FILE` nor
/// `SSL_CERT_DIR` chooses the trust source and the system has no certificates (`no_system_roots`,
/// asked only then). The variables count as `rustls-native-certs` reads them: any `SSL_CERT_FILE`
/// (even empty), or an `SSL_CERT_DIR` with a non-empty entry.
// Only Linux and the BSDs fall back; the rule is tested on every platform.
#[cfg_attr(not(all(unix, not(target_vendor = "apple"), not(target_os = "android"))), allow(dead_code))]
fn roots(file: Option<std::ffi::OsString>, dirs: Option<std::ffi::OsString>, no_system_roots: impl FnOnce() -> bool) -> RootCerts {
    let named = file.is_some() || dirs.is_some_and(|d| std::env::split_paths(&d).any(|p| !p.as_os_str().is_empty()));
    if !named && no_system_roots() { RootCerts::WebPki } else { RootCerts::PlatformVerifier }
}

#[cfg(test)]
mod tests {
    use super::{RootCerts, roots};

    #[test]
    fn bundled_roots_only_without_a_named_source_or_system_roots() {
        let s = |v: &str| Some(std::ffi::OsString::from(v));
        let bundled = |file, dirs| matches!(roots(file, dirs, || true), RootCerts::WebPki);
        assert!(bundled(None, None));
        assert!(matches!(roots(None, None, || false), RootCerts::PlatformVerifier));
        // An empty directory list is no list: the system's defaults (or the fallback) apply.
        assert!(bundled(None, s("")));
        // `:` separates directories only on Unix; on Windows the string is one directory.
        #[cfg(unix)]
        assert!(bundled(None, s("::")));
        assert!(!bundled(None, s("/etc/ssl/certs")));
        // An empty file is still a file the verifier is told to read.
        assert!(!bundled(s(""), None));
        // A named source is never widened, so the system isn't even asked.
        let asked = std::cell::Cell::new(false);
        let r = roots(s("ca.pem"), None, || {
            asked.set(true);
            true
        });
        assert!(matches!(r, RootCerts::PlatformVerifier) && !asked.get());
    }
}
