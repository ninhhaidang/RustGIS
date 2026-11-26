//! Android as a [`Platform`]: the system's file picker (the Storage Access Framework), asked for
//! through the activity's Java half (`java/org/rustgis/mobile/MainActivity.java`).
//!
//! Rust calls Java and never the other way: what happened is collected with `takeEvents`, so the
//! library exports nothing but `android_main`.

use crate::shell;
use jni::objects::{JObject, JString};
use jni::refs::Global;
use jni::{JValue, JavaVM, jni_sig, jni_str};
use rustgis_engine::platform::{Event, Platform};
use rustgis_ui_egui::i18n;
use std::cell::RefCell;
use std::sync::Arc;
use winit::platform::android::activity::AndroidApp;

/// The phone, through its activity.
pub struct Phone {
    activity: Arc<Global<JObject<'static>>>,
    /// Why the activity last couldn't be asked what happened, until it can be again: said once,
    /// not every frame.
    unreachable: RefCell<Option<String>>,
}

impl Phone {
    /// The phone `android` runs on, or why its Java half can't be reached (an APK built without
    /// it): the app then runs without a file picker and says so.
    pub fn new(android: &AndroidApp) -> Result<Phone, String> {
        let raw = android.activity_as_ptr().cast();
        let activity = JavaVM::singleton()
            .and_then(|vm| {
                vm.attach_current_thread(|env| {
                    // SAFETY: `android-activity` keeps this global reference to the activity alive
                    // for as long as `android_main` runs, and it is only borrowed here to make a
                    // global reference of our own.
                    #[expect(unsafe_code, reason = "see SAFETY above")]
                    let activity = unsafe { JObject::from_raw(env, raw) };
                    let held = env.new_global_ref(&activity);
                    // The borrowed reference is `android-activity`'s to delete, not ours.
                    let _ = activity.into_raw();
                    held
                })
            })
            .map_err(|e| i18n::fmt("Android's Java side can't be reached: {error}", &[("error", &e.to_string())]))?;
        let phone = Phone { activity: Arc::new(activity), unreachable: RefCell::new(None) };
        let owner = phone.java(|env, activity| {
            let token = env.call_method(activity, jni_str!("copyOwner"), jni_sig!(() -> JString), &[])?.l()?;
            env.cast_local::<JString>(token)?.try_to_string(env)
        })?;
        rustgis_ui_touch::set_copy_owner(owner)?;
        Ok(phone)
    }

    /// Tells the engine what Android allows (`rustgis_data::memory::set_phone`): the memory it
    /// has, and, asked again each time a job plans, what is free of it. Where Android doesn't say
    /// at start, [`shell::MEMORY_UNSAID`] stands in. Returns whether Android said.
    pub fn plan_memory(&self) -> bool {
        let activity = self.activity.clone();
        let total = memory(&activity).map(|(total, _)| total).filter(|total| *total > 0);
        let free = move || memory(&activity).map(|(_, free)| free);
        if !rustgis_engine::data::memory::set_phone(total.unwrap_or(shell::MEMORY_UNSAID), free) {
            log::warn!("the memory limits were read before Android's were set");
        }
        total.is_some()
    }

    /// Runs `call` with the Java environment and the activity, on this thread.
    fn java<T>(&self, call: impl FnOnce(&mut jni::Env, &JObject) -> jni::errors::Result<T>) -> Result<T, String> {
        JavaVM::singleton()
            .and_then(|vm| vm.attach_current_thread(|env| call(env, &self.activity)))
            .map_err(|e| i18n::fmt("Android refused: {error}", &[("error", &e.to_string())]))
    }

    /// The room the system's bars, a camera cutout and the soft keyboard take at the window's
    /// edges, in pixels (left, top, right, bottom), and whether the keyboard is up where Android
    /// says (11 and later). `None` before the window is laid out, or when the activity can't be
    /// asked.
    pub fn insets(&self) -> Option<([f32; 4], Option<bool>)> {
        let text = self
            .java(|env, activity| {
                let taken = env.call_method(activity, jni_str!("insets"), jni_sig!(() -> JString), &[])?.l()?;
                if taken.is_null() {
                    return Ok(String::new());
                }
                env.cast_local::<JString>(taken)?.try_to_string(env)
            })
            .ok()?;
        let fields: Vec<f32> = text.split(' ').filter_map(|edge| edge.parse().ok()).collect();
        let edges = <[f32; 4]>::try_from(fields.get(..4)?).ok()?;
        Some((edges, fields.get(4).map(|up| *up > 0.5)))
    }

    /// Draws the system bars' icons dark (`light` ground under them) or light (a dark one).
    pub fn bars_light(&self, light: bool) {
        let asked = self.java(|env, activity| {
            env.call_method(activity, jni_str!("barsLight"), jni_sig!((light: boolean) -> void), &[JValue::Bool(light)])?.v()
        });
        if let Err(e) = asked {
            log::warn!("{e}");
        }
    }

    /// Whether a picker for opening is on the screen and hasn't answered.
    pub fn picking(&self) -> bool {
        self.java(|env, activity| env.call_method(activity, jni_str!("picking"), jni_sig!(() -> boolean), &[])?.z()).unwrap_or(false)
    }

    /// Whether something asked for is still to be reported: the window keeps asking while it is.
    pub fn busy(&self) -> bool {
        self.java(|env, activity| env.call_method(activity, jni_str!("busy"), jni_sig!(() -> boolean), &[])?.z()).unwrap_or(false)
    }
}

/// The memory Android has and what is free of it (`MainActivity.memory`), from any thread; `None`
/// when it can't say.
fn memory(activity: &JObject) -> Option<(u64, u64)> {
    let text = JavaVM::singleton()
        .and_then(|vm| {
            vm.attach_current_thread(|env| -> jni::errors::Result<String> {
                let said = env.call_method(activity, jni_str!("memory"), jni_sig!(() -> JString), &[])?.l()?;
                env.cast_local::<JString>(said)?.try_to_string(env)
            })
        })
        .ok()?;
    let (total, free) = text.split_once(' ')?;
    Some((total.parse().ok()?, free.parse().ok()?))
}

impl Platform for Phone {
    fn pick_files(&self, purpose: &str) -> Result<(), String> {
        self.java(|env, activity| {
            let purpose = env.new_string(purpose)?;
            env.call_method(activity, jni_str!("pick"), jni_sig!((purpose: JString) -> void), &[JValue::Object(&purpose)])?.v()
        })
    }

    fn save_file(&self, name: &str, bytes: Vec<u8>, project: bool) -> Result<(), String> {
        self.java(|env, activity| {
            let name = env.new_string(name)?;
            let bytes = env.byte_array_from_slice(&bytes)?;
            env.call_method(
                activity,
                jni_str!("save"),
                jni_sig!((name: JString, bytes: [jbyte], project: boolean) -> void),
                &[JValue::Object(&name), JValue::Object(&bytes), JValue::Bool(project)],
            )?
            .v()
        })
    }

    fn write_back(&self, document: &str, name: &str, bytes: Vec<u8>) -> Result<(), String> {
        self.java(|env, activity| {
            let document = env.new_string(document)?;
            let name = env.new_string(name)?;
            let bytes = env.byte_array_from_slice(&bytes)?;
            env.call_method(
                activity,
                jni_str!("writeBack"),
                jni_sig!((document: JString, name: JString, bytes: [jbyte]) -> void),
                &[JValue::Object(&document), JValue::Object(&name), JValue::Object(&bytes)],
            )?
            .v()
        })
    }

    fn locate(&self, on: bool) -> Result<(), String> {
        self.java(|env, activity| env.call_method(activity, jni_str!("locate"), jni_sig!((on: boolean) -> void), &[JValue::Bool(on)])?.v())
    }

    fn keep_locating(&self, on: bool) -> Result<(), String> {
        self.java(|env, activity| {
            env.call_method(activity, jni_str!("keepLocating"), jni_sig!((on: boolean) -> void), &[JValue::Bool(on)])?.v()
        })
    }

    fn take_photo(&self) -> Result<(), String> {
        self.java(|env, activity| env.call_method(activity, jni_str!("takePhoto"), jni_sig!(() -> void), &[])?.v())
    }

    fn take_events(&self) -> Vec<Event> {
        let lines = self.java(|env, activity| {
            let taken = env.call_method(activity, jni_str!("takeEvents"), jni_sig!(() -> JString), &[])?.l()?;
            if taken.is_null() {
                return Ok(String::new());
            }
            env.cast_local::<JString>(taken)?.try_to_string(env)
        });
        match lines {
            Ok(lines) => {
                self.unreachable.replace(None);
                lines.lines().map(Event::parse).collect()
            }
            // Asked every frame: the same failure is said once.
            Err(e) if self.unreachable.borrow().as_deref() == Some(e.as_str()) => Vec::new(),
            Err(e) => {
                self.unreachable.replace(Some(e.clone()));
                vec![Event::Failed(e)]
            }
        }
    }
}
