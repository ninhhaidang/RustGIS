//! The Android app: a `NativeActivity` whose Java half ([`Phone`]) shows the system's pickers,
//! finds the device's position and asks the camera.

use crate::phone::Phone;
use crate::shell;
use eframe::egui;
use eframe::wgpu;
use rustgis_engine::data::dirs;
use winit::platform::android::activity::AndroidApp;

impl shell::Phone for Phone {
    fn insets(&self, pixels_per_point: f32) -> Option<(egui::SafeAreaInsets, Option<bool>)> {
        let (edges, keyboard) = Phone::insets(self)?;
        let [left, top, right, bottom] = edges.map(|edge| edge.max(0.0) / pixels_per_point.max(0.1));
        Some((egui::SafeAreaInsets(egui::epaint::MarginF32 { left, top, right, bottom }), keyboard))
    }

    fn bars_light(&self, light: bool, _frame: &eframe::Frame) {
        Phone::bars_light(self, light);
    }

    fn busy(&self) -> bool {
        Phone::busy(self)
    }

    fn picking(&self) -> bool {
        Phone::picking(self)
    }
}

/// Points the app's folders (`rustgis_data::dirs`) at the app's own storage: Android has no
/// `HOME`, so without this there are no settings, no tile cache and no recovery folder.
/// Returns whether there is such storage.
///
/// Run first in [`android_main`], before anything asks for a folder.
fn set_folders(app: &AndroidApp) -> bool {
    match app.internal_data_path() {
        Some(data) => dirs::set_root(data),
        None => false,
    }
}

/// Android's entry point for a `NativeActivity` (`android-activity` calls it on its own thread).
// `no_mangle` is the one unsafe attribute here: the name is what `android-activity` calls.
#[expect(unsafe_code, reason = "`no_mangle`: `android-activity` calls this name")]
#[unsafe(no_mangle)]
fn android_main(android: AndroidApp) {
    android_logger::init_once(android_logger::Config::default().with_max_level(log::LevelFilter::Info).with_tag("rustgis"));
    let folders = set_folders(&android);
    let mut app = shell::front_end(folders);
    let phone = Phone::new(&android);
    // The app's external files folder (`/sdcard/Android/data/org.rustgis.mobile/files`), where
    // `adb push` can put files.
    let pushed = shell::pushed(android.external_data_path(), &mut app);
    let mut wgpu_options = eframe::egui_wgpu::WgpuConfiguration::default();
    if let eframe::egui_wgpu::WgpuSetup::CreateNew(setup) = &mut wgpu_options.wgpu_setup {
        // Vulkan where the phone has it, OpenGL ES otherwise (older phones, the emulator).
        setup.instance_descriptor.backends = wgpu::Backends::VULKAN | wgpu::Backends::GL;
    }
    let options = eframe::NativeOptions { android_app: Some(android), wgpu_options, ..Default::default() };
    if let Err(e) = shell::run(app, phone, pushed, options) {
        log::error!("the app stopped: {e}");
    }
    // The activity is gone (Back, or the system destroyed it). winit's event loop can't be
    // made twice in a process, so a process left alive would answer the next launch with a
    // blank window: end it, and the next launch starts afresh. Phase 1 keeps the session
    // across this instead (`onSaveInstanceState`).
    std::process::exit(0);
}
