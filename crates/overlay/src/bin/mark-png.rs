//! Render the menu-bar mark exactly as the status item does, to a PNG.
//!
//! The ASCII preview used while designing this tested the GEOMETRY: my own
//! point-in-polygon over the same points. It cannot catch a mistake in the
//! drawing, and the drawing is where the risk lives: the sockets are cut by
//! an even-odd winding rule, and getting that wrong yields a skull with no
//! eyes that every geometry test still passes.
//!
//! This goes through NSBezierPath and NSImage, the real path, and writes a
//! PNG that can be opened.
//!
//! Usage: cargo run -p overlay --bin mark-png --features display -- /tmp/mark.png

fn main() -> anyhow::Result<()> {
    #[cfg(all(target_os = "macos", feature = "display"))]
    {
        use objc2::rc::autoreleasepool;
        use objc2::AllocAnyThread;
        use objc2_app_kit::{
            NSBezierPath, NSBitmapImageFileType, NSBitmapImageRep, NSColor, NSGraphicsContext,
            NSImage, NSWindingRule,
        };
        use objc2_foundation::{NSPoint, NSSize};

        let out = std::env::args()
            .nth(1)
            .unwrap_or_else(|| "/tmp/mark.png".to_string());
        // Bigger than the 15pt glyph so the shape is inspectable; the same
        // unit design scaled, so what renders here is what renders there.
        let px: f64 = std::env::args()
            .nth(2)
            .and_then(|s| s.parse().ok())
            .unwrap_or(240.0);

        autoreleasepool(|_| unsafe {
            let m = overlay::mark::mark_in(px);
            let size = NSSize::new(px, px);
            let image = NSImage::initWithSize(NSImage::alloc(), size);
            // Same deprecated-but-correct pair status_item.rs uses, and for
            // the same reason documented there: the block-based API draws
            // into a context whose flippedness we do not control, and this
            // tool must render through the IDENTICAL path as the real status
            // item or it proves nothing about it.
            #[allow(deprecated)]
            image.lockFocusFlipped(true);

            // Opaque black background so the white mark is visible and any
            // hole shows as black rather than as transparency the viewer
            // might render white anyway.
            NSColor::blackColor().setFill();
            let bg = NSBezierPath::bezierPathWithRect(objc2_foundation::NSRect::new(
                NSPoint::new(0.0, 0.0),
                size,
            ));
            bg.fill();

            NSColor::whiteColor().setFill();
            let path = NSBezierPath::bezierPath();
            let sub = |pts: &[overlay::layout::Point]| {
                for (i, p) in pts.iter().enumerate() {
                    let at = NSPoint::new(p.x, p.y);
                    if i == 0 {
                        path.moveToPoint(at);
                    } else {
                        path.lineToPoint(at);
                    }
                }
                path.closePath();
            };
            sub(&m.outline);
            for hole in &m.holes {
                sub(hole);
            }
            path.setWindingRule(NSWindingRule::EvenOdd);
            path.fill();

            let ctx = NSGraphicsContext::currentContext().unwrap();
            #[allow(deprecated)]
            let rep = NSBitmapImageRep::initWithFocusedViewRect(
                NSBitmapImageRep::alloc(),
                objc2_foundation::NSRect::new(NSPoint::new(0.0, 0.0), size),
            )
            .unwrap();
            let _ = ctx;
            #[allow(deprecated)]
            image.unlockFocus();

            let data = rep
                .representationUsingType_properties(
                    NSBitmapImageFileType::PNG,
                    &objc2_foundation::NSDictionary::new(),
                )
                .unwrap();
            std::fs::write(&out, data.to_vec()).unwrap();
            println!("wrote {out} ({px}x{px})");
        });
    }

    #[cfg(not(all(target_os = "macos", feature = "display")))]
    {
        eprintln!("mark-png needs macOS with --features display");
    }
    Ok(())
}
