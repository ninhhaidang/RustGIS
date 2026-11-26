//! The answers of Android's secret store (`java/…/SecretVault.java`), read apart from the store
//! itself so the host's tests cover them.

use rustgis_engine::data::secrets::{Secret, SecretError};
use rustgis_ui_egui::{i18n, tl};

/// Reads the Java half's answer: `ok` (with the secret after a tab, for a load that found one),
/// `none`, `unavailable` with the system's reason after a tab, or `failed` with a code after a
/// tab and the system's reason, if any, after another ([`failure`]).
pub(crate) fn answer(line: &str) -> Result<Option<Secret>, SecretError> {
    match line.split_once('\t') {
        Some(("ok", secret)) => Ok(Some(Secret::new(secret))),
        Some(("unavailable", why)) => Err(SecretError::Unavailable(why.to_string())),
        Some(("failed", why)) => Err(SecretError::Failed(failure(why))),
        None if line == "ok" || line == "none" => Ok(None),
        _ => Err(SecretError::Failed("the Android keystore gave an answer RustGIS doesn't know".into())),
    }
}

/// A failure's reason in the user's language, from the Java half's code and the system's reason.
/// A code this version doesn't know is passed on as it came.
fn failure(why: &str) -> String {
    let (code, detail) = why.split_once('\t').unwrap_or((why, ""));
    let text = match code {
        "newer" => tl!("it was kept by a newer RustGIS: update RustGIS to use it"),
        "unwritten" => tl!("the app's storage can't be written to"),
        "damaged" => tl!("the kept secret is damaged"),
        "no-key" => tl!("the kept secret can't be decrypted: the keystore no longer has its key"),
        "undecryptable" => tl!("the kept secret can't be decrypted"),
        "error" if !detail.is_empty() => return detail.to_string(),
        _ => return why.to_string(),
    };
    if detail.is_empty() { text.to_string() } else { i18n::fmt("{reason}: {detail}", &[("reason", text), ("detail", detail)]) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_answer_of_the_java_half_is_read() {
        assert_eq!(answer("ok\tsk-test").unwrap().map(|s| s.expose().to_string()), Some("sk-test".into()));
        assert!(answer("ok").unwrap().is_none());
        assert!(answer("none").unwrap().is_none());
        assert_eq!(answer("unavailable\tno keystore"), Err(SecretError::Unavailable("no keystore".into())));
        assert_eq!(answer("failed\tdamaged"), Err(SecretError::Failed("the kept secret is damaged".into())));
        assert_eq!(answer("failed\tunwritten\tENOSPC"), Err(SecretError::Failed("the app's storage can't be written to: ENOSPC".into())));
        assert_eq!(answer("failed\terror\tBadPaddingException"), Err(SecretError::Failed("BadPaddingException".into())));
        assert_eq!(answer("failed\tlater code"), Err(SecretError::Failed("later code".into())));
        assert!(matches!(answer("saved"), Err(SecretError::Failed(why)) if why.contains("doesn't know")));
        assert!(matches!(answer("later\tx"), Err(SecretError::Failed(why)) if why.contains("doesn't know")));
    }
}
