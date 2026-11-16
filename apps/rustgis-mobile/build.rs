//! Draws the app's icons from the logo's code (`crates/render/src/logo.rs`), the same code as the
//! desktop's window icon, so there is no image file to keep in sync.
//!
//! - On iOS, `$OUT_DIR/AppIcon.png`, `AppIcon-Dark.png` and `AppIcon-Tinted.png`, 1024 pixels
//!   square, for `build-ios.sh` to put in the bundle: the logo on white (opaque, as the App Store
//!   requires of an app icon), on the dark background iOS supplies, and in one colour for iOS to
//!   tint.
//! - On Android, `$OUT_DIR/res`, the resources `build-apk.sh` links into the APK: an adaptive icon
//!   (`mipmap/ic_launcher`) of the logo on white, with a monochrome layer that themed icons (Android
//!   13 and later, and launchers that tint icons) draw in their own colour.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[expect(dead_code, reason = "the build script uses only the pixmaps")]
#[path = "../../crates/render/src/logo.rs"]
mod logo;

use std::path::Path;

type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

/// The iOS icon's size, in pixels: the one size the App Store asks for.
const IOS_SIZE: u32 = 1024;
/// The logo's size within it: iOS rounds the icon's corners, which would cut the cog's teeth.
const IOS_LOGO: u32 = 800;

/// Android's adaptive icon layers are 108 dp square, of which a launcher shows the middle 72 dp
/// at most, cut to its own shape, and only the middle 66 dp circle for certain.
const LAYER_DP: u32 = 108;
/// The logo's size on the foreground layer: the cog, 96% of it, fits the 66 dp circle.
const LOGO_DP: u32 = 64;
/// The logo's size on the monochrome layer: smaller, as themed icons are drawn.
const MONO_DP: u32 = 56;
/// Android's screen densities, with their scale over 1 px per dp.
const DENSITIES: [(&str, u32); 5] = [("mdpi", 4), ("hdpi", 6), ("xhdpi", 8), ("xxhdpi", 12), ("xxxhdpi", 16)];

fn main() -> Result {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../../crates/render/src/logo.rs");
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR")?);
    match std::env::var("CARGO_CFG_TARGET_OS").as_deref() {
        Ok("ios") => ios(&out),
        Ok("android") => android(&out),
        _ => Ok(()),
    }
}

fn ios(out: &Path) -> Result {
    let white = tiny_skia::Color::WHITE;
    let logo = logo::pixmap(IOS_LOGO).ok_or("could not draw the logo")?;
    let mono = logo::monochrome(IOS_LOGO).ok_or("could not draw the logo")?;
    // RGB, with no alpha channel: App Store Connect refuses an icon that has one.
    write_rgb(&centred(IOS_SIZE, &logo, Some(white))?, &out.join("AppIcon.png"))?;
    // The dark and tinted icons may be clear: iOS draws them on a background of its own.
    centred(IOS_SIZE, &logo, None)?.save_png(out.join("AppIcon-Dark.png"))?;
    centred(IOS_SIZE, &mono, None)?.save_png(out.join("AppIcon-Tinted.png"))?;
    Ok(())
}

fn android(out: &Path) -> Result {
    let res = out.join("res");
    // A fresh folder: nothing left from an older build is linked in.
    if res.exists() {
        std::fs::remove_dir_all(&res)?;
    }
    for (density, px4) in DENSITIES {
        let dir = res.join(format!("mipmap-{density}"));
        std::fs::create_dir_all(&dir)?;
        let px = |dp: u32| dp * px4 / 4;
        let logo = logo::pixmap(px(LOGO_DP)).ok_or("could not draw the logo")?;
        let mono = logo::monochrome(px(MONO_DP)).ok_or("could not draw the logo")?;
        centred(px(LAYER_DP), &logo, None)?.save_png(dir.join("ic_launcher_foreground.png"))?;
        centred(px(LAYER_DP), &mono, None)?.save_png(dir.join("ic_launcher_monochrome.png"))?;
    }
    // Adaptive icons need Android 8.0 (API 26), the oldest the app runs on.
    let dir = res.join("mipmap-anydpi-v26");
    std::fs::create_dir_all(&dir)?;
    std::fs::write(
        dir.join("ic_launcher.xml"),
        r#"<?xml version="1.0" encoding="utf-8"?>
<!-- Drawn by build.rs from the logo's code. -->
<adaptive-icon xmlns:android="http://schemas.android.com/apk/res/android">
    <background android:drawable="@android:color/white" />
    <foreground android:drawable="@mipmap/ic_launcher_foreground" />
    <monochrome android:drawable="@mipmap/ic_launcher_monochrome" />
</adaptive-icon>
"#,
    )?;
    Ok(())
}

/// `image` in the middle of a `size`-pixel square, filled with `background` or left clear.
fn centred(size: u32, image: &tiny_skia::Pixmap, background: Option<tiny_skia::Color>) -> Result<tiny_skia::Pixmap> {
    let mut canvas = tiny_skia::Pixmap::new(size, size).ok_or("could not make the icon's canvas")?;
    if let Some(colour) = background {
        canvas.fill(colour);
    }
    let at = (size.saturating_sub(image.width()) / 2) as i32;
    canvas.draw_pixmap(at, at, image.as_ref(), &tiny_skia::PixmapPaint::default(), tiny_skia::Transform::identity(), None);
    Ok(canvas)
}

/// An opaque pixmap as an RGB PNG.
fn write_rgb(image: &tiny_skia::Pixmap, path: &Path) -> Result {
    let rgb: Vec<u8> = image.pixels().iter().flat_map(|p| [p.red(), p.green(), p.blue()]).collect();
    let file = std::io::BufWriter::new(std::fs::File::create(path)?);
    let mut encoder = png::Encoder::new(file, image.width(), image.height());
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.write_header()?.write_image_data(&rgb)?;
    Ok(())
}
