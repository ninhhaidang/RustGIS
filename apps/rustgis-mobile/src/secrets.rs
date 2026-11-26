//! Android's secret store, [`Vault`]: the activity's Java half (`java/…/SecretVault.java`) keeps
//! each secret encrypted with AES-GCM, under a key the Android Keystore makes once and never lets
//! out, in the app's private storage.

use crate::vaultanswer::answer;
use jni::objects::{JObject, JString};
use jni::refs::Global;
use jni::{JValue, JavaVM, jni_sig, jni_str};
use rustgis_engine::data::secrets::{Secret, SecretError, SecretStore, check_account};

/// `Services::secrets` on Android. When the keystore can't be used, it answers
/// [`SecretError::Unavailable`], and a secret then lasts for the session, as on a computer with
/// no keychain: nothing is kept unencrypted.
pub struct Vault {
    activity: Global<JObject<'static>>,
}

impl Vault {
    /// The store, through the app's `activity`.
    pub fn new(activity: Global<JObject<'static>>) -> Vault {
        Vault { activity }
    }

    /// Runs `call` with the Java environment and the activity, on any thread: the methods are the
    /// activity's, so they are found through its class loader, which a class looked up by name
    /// from a thread Rust started wouldn't use. Returns the Java half's answer, one line.
    fn ask(&self, call: impl FnOnce(&mut jni::Env, &JObject) -> jni::errors::Result<String>) -> Result<String, SecretError> {
        JavaVM::singleton()
            .and_then(|vm| vm.attach_current_thread(|env| call(env, &self.activity)))
            .map_err(|e| SecretError::Unavailable(e.to_string()))
    }
}

impl SecretStore for Vault {
    /// Decrypts the account's secret; one the keystore's key no longer opens is a failure.
    fn load(&self, account: &str) -> Result<Option<Secret>, SecretError> {
        check_account(account)?;
        let line = self.ask(|env, activity| {
            let account = env.new_string(account)?;
            let line = env
                .call_method(activity, jni_str!("loadSecret"), jni_sig!((account: JString) -> JString), &[JValue::Object(&account)])?
                .l()?;
            env.cast_local::<JString>(line)?.try_to_string(env)
        })?;
        answer(&line)
    }

    /// Encrypts the secret under a new nonce, bound to its account, and keeps it.
    fn save(&self, account: &str, value: &Secret) -> Result<(), SecretError> {
        check_account(account)?;
        let line = self.ask(|env, activity| {
            let account = env.new_string(account)?;
            let value = env.new_string(value.expose())?;
            let line = env
                .call_method(
                    activity,
                    jni_str!("saveSecret"),
                    jni_sig!((account: JString, value: JString) -> JString),
                    &[JValue::Object(&account), JValue::Object(&value)],
                )?
                .l()?;
            env.cast_local::<JString>(line)?.try_to_string(env)
        })?;
        answer(&line).map(|_| ())
    }

    /// Forgets the account's secret; the keystore's key stays for the others.
    fn delete(&self, account: &str) -> Result<(), SecretError> {
        check_account(account)?;
        let line = self.ask(|env, activity| {
            let account = env.new_string(account)?;
            let line = env
                .call_method(activity, jni_str!("deleteSecret"), jni_sig!((account: JString) -> JString), &[JValue::Object(&account)])?
                .l()?;
            env.cast_local::<JString>(line)?.try_to_string(env)
        })?;
        answer(&line).map(|_| ())
    }
}
