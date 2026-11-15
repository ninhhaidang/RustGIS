//! On iOS, draws the app's icon: the RustGIS logo on white, 1024 pixels square, written to
//! `$OUT_DIR/AppIcon.png` for `build-ios.sh` to put in the bundle. It is drawn by the same code
//! as the desktop's window icon (`crates/render/src/logo.rs`), so there is no image file to keep
//! in sync. Opaque, as the App Store requires of an app icon.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[expect(dead_code, reason = "the build script uses only the pixmap")]
#[path = "../../crates/render/src/logo.rs"]
mod logo;

/// The icon's size, in pixels: the one size the App Store asks for.
const SIZE: u32 = 1024;
/// The logo's size within it: iOS rounds the icon's corners, which would cut the cog's teeth.
const LOGO: u32 = 800;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../../crates/render/src/logo.rs");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("ios") {
        return Ok(());
    }
    let mut icon = tiny_skia::Pixmap::new(SIZE, SIZE).ok_or("could not make the icon's canvas")?;
    icon.fill(tiny_skia::Color::WHITE);
    let logo = logo::pixmap(LOGO).ok_or("could not draw the logo")?;
    let at = ((SIZE - LOGO) / 2) as i32;
    icon.draw_pixmap(at, at, logo.as_ref(), &tiny_skia::PixmapPaint::default(), tiny_skia::Transform::identity(), None);
    // RGB, with no alpha channel: App Store Connect refuses an icon that has one.
    let rgb: Vec<u8> = icon.pixels().iter().flat_map(|p| [p.red(), p.green(), p.blue()]).collect();
    let path = std::path::PathBuf::from(std::env::var("OUT_DIR")?).join("AppIcon.png");
    let file = std::io::BufWriter::new(std::fs::File::create(&path)?);
    let mut encoder = png::Encoder::new(file, SIZE, SIZE);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.write_header()?.write_image_data(&rgb)?;
    Ok(())
}
