//! Render the menu-bar cat glyph the way the status item draws it, at 1x
//! and at a magnified size, and save PNGs for review.
//!
//! Exists because the status item's own image lives in the menu bar, which
//! needs a Screen Recording grant to capture; this draws the same geometry
//! through the same NSImage/lockFocus path and writes it where eyes can
//! reach it. Run: `cargo run -p overlay --example glyph_capture`.

fn main() -> anyhow::Result<()> {
    #[cfg(all(target_os = "macos", feature = "display"))]
    return capture::run();

    #[cfg(not(all(target_os = "macos", feature = "display")))]
    {
        eprintln!("glyph_capture: needs macOS and the `display` feature.");
        Ok(())
    }
}

#[cfg(all(target_os = "macos", feature = "display"))]
mod capture {
    use objc2::rc::Retained;
    use objc2::AllocAnyThread;
    use objc2_app_kit::{NSBezierPath, NSBitmapImageFileType, NSBitmapImageRep, NSColor, NSImage};
    use objc2_foundation::{NSDictionary, NSPoint, NSSize, NSString};

    pub fn run() -> anyhow::Result<()> {
        // 15pt (ship size) and 60pt (inspection size), white on transparent
        // like the dark-menu-bar rendering.
        for (size, name) in [(15.0f64, "glyph_15pt"), (60.0f64, "glyph_60pt")] {
            let image = NSImage::initWithSize(NSImage::alloc(), NSSize::new(size, size));
            #[allow(deprecated)]
            image.lockFocusFlipped(true);
            NSColor::whiteColor().setFill();
            for poly in overlay::cat::glyph_in(size) {
                let path = NSBezierPath::bezierPath();
                for (i, p) in poly.iter().enumerate() {
                    let at = NSPoint::new(p.x, p.y);
                    if i == 0 {
                        path.moveToPoint(at);
                    } else {
                        path.lineToPoint(at);
                    }
                }
                path.closePath();
                path.fill();
            }
            #[allow(deprecated)]
            image.unlockFocus();

            let rep = unsafe {
                let data = image
                    .TIFFRepresentation()
                    .ok_or_else(|| anyhow::anyhow!("no tiff"))?;
                NSBitmapImageRep::imageRepWithData(&data)
                    .ok_or_else(|| anyhow::anyhow!("no bitmap rep"))?
            };
            let rep: Retained<NSBitmapImageRep> = rep;
            let props: Retained<NSDictionary<_, objc2::runtime::AnyObject>> = NSDictionary::new();
            let png = unsafe {
                rep.representationUsingType_properties(NSBitmapImageFileType::PNG, &props)
                    .ok_or_else(|| anyhow::anyhow!("png encode failed"))?
            };
            let path = format!("/tmp/{name}.png");
            unsafe {
                png.writeToFile_atomically(&NSString::from_str(&path), true);
            }
            println!("{path}");
        }
        Ok(())
    }
}
