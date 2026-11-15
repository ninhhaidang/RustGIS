//! The iOS app's executable (#1902): UIKit starts it, and [`rustgis_mobile::ios_main`] runs the
//! app. On every other target it does nothing: Android loads the library instead.

fn main() {
    #[cfg(target_os = "ios")]
    rustgis_mobile::ios_main();
}
