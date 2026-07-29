//! The macOS menu-bar presence: an `NSStatusItem` whose glyph tracks the
//! product state and whose menu is rebuilt from a [`MenuModel`].
//!
//! Why this exists at all: `OutLoud.app` sets `LSUIElement`, so it has no Dock
//! icon and no window. Without a status item a running daemon is completely
//! invisible — there is no way to tell it is alive, no way to configure it,
//! and no way to quit it except `killall`. `docs/ux/05-settings-and-states.md`
//! always assumed this surface ("the tray glyph"); this is it.
//!
//! Three properties that must stay true, gathered here so they can be
//! audited together:
//!
//! 1. **`LSUIElement` / `Accessory` activation policy stays.** A status item
//!    works fine in an accessory app, and the app must never activate:
//!    dictation writes into whatever field the user is focused on, so
//!    stealing focus would break the core feature. Opening the *menu* is the
//!    one moment AppKit legitimately takes event focus, and it hands it back
//!    on dismiss without the app ever becoming active.
//! 2. **The glyph is a template SF Symbol.** Template images are recolored by
//!    the system for light/dark menu bars and for the highlighted state; a
//!    baked PNG is not. If symbol lookup ever fails we fall back to a text
//!    title, because an *empty* status item is the bug this module fixes.
//! 3. **Clicks are queued, never executed here.** The action callback pushes
//!    a [`MenuId`] onto a queue that the host drains from its own loop. The
//!    overlay crate stays free of policy (config writes, permission
//!    deep-links, quit), and a slow action can never block AppKit's menu
//!    tracking.

use std::cell::RefCell;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Sel};
use objc2::{
    define_class, msg_send, sel, AllocAnyThread, DefinedClass, MainThreadMarker, MainThreadOnly,
};
use objc2_app_kit::{
    NSBezierPath, NSColor, NSControlStateValueOff, NSControlStateValueOn, NSImage, NSLineCapStyle,
    NSLineJoinStyle, NSMenu, NSMenuItem, NSStatusBar, NSStatusItem, NSVariableStatusItemLength,
};
use objc2_foundation::{NSObject, NSPoint, NSSize, NSString};

use crate::mark;
use crate::menu::{glyph_tint, MenuId, MenuItem, MenuModel};
use crate::theme::Color;

/// The one place this crate converts theme data into an AppKit color.
/// `theme` stays pure so it compiles headless (see its module doc), which
/// means the conversion has to live on the AppKit side of the gate.
fn ns_color(c: Color) -> Retained<NSColor> {
    NSColor::colorWithSRGBRed_green_blue_alpha(c.r, c.g, c.b, c.a)
}

define_class!(
    /// The Objective-C target every menu item points at. It owns the click
    /// queue so the callback does nothing but record which id fired, which
    /// is what keeps menu tracking responsive no matter how slow the host's
    /// handler is.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "OutLoudStatusTarget"]
    #[ivars = RefCell<Vec<MenuId>>]
    struct StatusTarget;

    impl StatusTarget {
        #[unsafe(method(aquaMenuAction:))]
        fn menu_action(&self, sender: &NSMenuItem) {
            // The tag is the MenuId the host handed us, round-tripped
            // through AppKit untouched. Negative tags cannot occur (ids are
            // u64 truncated to isize by construction below), but clamp
            // rather than wrap so a future bug is inert instead of firing
            // some other action.
            let tag = sender.tag();
            if tag >= 0 {
                self.ivars().borrow_mut().push(MenuId(tag as u64));
            }
        }
    }
);

impl StatusTarget {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(RefCell::new(Vec::new()));
        unsafe { msg_send![super(this), init] }
    }
}

/// The live status item. Dropping it removes the item from the menu bar.
pub struct MacStatusItem {
    item: Retained<NSStatusItem>,
    target: Retained<StatusTarget>,
    mtm: MainThreadMarker,
    /// The last model applied, so the common case (nothing changed since the
    /// last 30Hz tick) costs one comparison instead of rebuilding an NSMenu
    /// while the user may have it open — rebuilding an open menu closes it,
    /// which would make the menu impossible to click.
    applied: Option<MenuModel>,
    /// The appearance the glyph was last drawn for. The mark is drawn in an
    /// explicit colour (see `mark_image`), so a light/dark switch must force
    /// a redraw even when the model itself is unchanged — otherwise the
    /// glyph keeps the old bar's colour and turns invisible on the new one.
    applied_dark: Option<bool>,
}

impl MacStatusItem {
    /// Create the status item. Must run on the main thread (AppKit).
    pub fn new(mtm: MainThreadMarker) -> anyhow::Result<Self> {
        let bar = NSStatusBar::systemStatusBar();
        // Variable length: the item is glyph-only today, but a future
        // "listening 0:07" title should be able to widen it.
        let item = bar.statusItemWithLength(NSVariableStatusItemLength);
        let target = StatusTarget::new(mtm);
        Ok(MacStatusItem {
            item,
            target,
            mtm,
            applied: None,
            applied_dark: None,
        })
    }

    /// Push a model to the menu bar. Cheap and idempotent: identical models
    /// are ignored, so the host can call this every frame.
    pub fn apply(&mut self, model: &MenuModel) {
        let dark = self.is_dark_menu_bar();
        if self.applied.as_ref() == Some(model) && self.applied_dark == Some(dark) {
            return;
        }
        self.set_glyph(model, dark);
        // The menu is appearance-independent; rebuild it only when the model
        // actually changed, because rebuilding closes an open menu.
        if self.applied.as_ref() != Some(model) {
            let menu = self.build_menu(&model.items);
            self.item.setMenu(Some(&menu));
        }
        self.applied = Some(model.clone());
        self.applied_dark = Some(dark);
    }

    /// Take everything the user clicked since the last call. The host maps
    /// ids to actions; this crate never does.
    pub fn drain_clicks(&self) -> Vec<MenuId> {
        std::mem::take(&mut *self.target.ivars().borrow_mut())
    }

    /// Whether the status item currently sits in a dark menu bar.
    ///
    /// Asked of the BUTTON's `effectiveAppearance`, not the app's: the menu
    /// bar can be dark while the app appearance is light (dynamic desktop,
    /// "allow wallpaper tinting" edge cases), and the button is the view
    /// that actually lives in the bar. Raw `msg_send` because this crate's
    /// objc2-app-kit feature set does not include NSAppearance, and one
    /// string comparison does not justify widening it.
    fn is_dark_menu_bar(&self) -> bool {
        let Some(button) = self.item.button(self.mtm) else {
            return false;
        };
        let appearance: Option<Retained<AnyObject>> =
            unsafe { msg_send![&*button, effectiveAppearance] };
        let Some(appearance) = appearance else {
            return false;
        };
        let name: Retained<NSString> = unsafe { msg_send![&*appearance, name] };
        name.to_string().contains("Dark")
    }

    fn set_glyph(&self, model: &MenuModel, dark: bool) {
        let Some(button) = self.item.button(self.mtm) else {
            // No button means no visible item; there is nothing we can do
            // about it here, and the menu is still attached to the item.
            return;
        };
        let tint = glyph_tint(model.state);
        let image = self.mark_image(tint, dark);
        // The accessibility description is the state, not the shape: a
        // VoiceOver user needs "OutLoud: listening", not "megaphone".
        image.setAccessibilityDescription(Some(&NSString::from_str(&model.tooltip)));
        button.setImage(Some(&image));
        // The colour is baked into the drawing, so no tint is applied here.
        //
        // The template route looks right and does not work for a
        // hand-drawn image: a template renders from the image's ALPHA
        // channel, and stroking black into a lock-focus context did not
        // give the system a mask it would draw, so the item was present,
        // correctly sized, reported by the accessibility API, and
        // completely invisible. Explicit colour is less clever and is
        // actually on screen, which is the entire job of this surface.
        button.setContentTintColor(None);
        button.setTitle(&NSString::from_str(""));
        button.setToolTip(Some(&NSString::from_str(&model.tooltip)));
    }

    /// Draw the megaphone mark into an `NSImage`.
    ///
    /// Drawn rather than loaded: the geometry in `crate::mark` is the
    /// single source shared with the Windows tray backend (an SF Symbol
    /// would break that, see mark.rs's module doc), and a shipped PNG
    /// would be wrong at some combination of Retina, menu bar height, and
    /// the user's menu bar size setting.
    ///
    /// `lockFocusFlipped` rather than `imageWithSize:flipped:drawingHandler:`
    /// because the block form produced a correctly-sized image that never
    /// painted: the handler ran (verified by printing from inside it) and the
    /// status item stayed blank. Locking focus draws into the image's own
    /// representation immediately and synchronously, which is both easier to
    /// reason about and the thing that actually works here.
    ///
    /// Flipped so the handler's coordinates match this crate's
    /// top-left-origin convention, which is what `mark::path_in` returns.
    fn mark_image(&self, tint: Option<Color>, dark: bool) -> Retained<NSImage> {
        let size = NSSize::new(mark::GLYPH_SIZE, mark::GLYPH_SIZE);
        let image = NSImage::initWithSize(NSImage::alloc(), size);
        #[allow(deprecated)]
        image.lockFocusFlipped(true);

        let m = mark::mark_in(mark::GLYPH_SIZE);
        // Drawn in its final colour. Untinted states use explicit white on a
        // dark bar and near-black on a light one, decided from the button's
        // real appearance in `is_dark_menu_bar`. NOT `labelColor`: a dynamic
        // colour resolves against the drawing context's current appearance,
        // which inside `lockFocus` is whatever the app last cached, so after
        // a light/dark switch it drew the OLD bar's colour — a white glyph
        // on a white menu bar, verified by screenshot, while every
        // programmatic check passed. Same trap family as 64af502.
        let colour = match tint {
            Some(c) => ns_color(c),
            None if dark => NSColor::whiteColor(),
            // 0.15 grey rather than pure black, matching how menu bar
            // template glyphs render in the light appearance.
            None => NSColor::colorWithSRGBRed_green_blue_alpha(0.15, 0.15, 0.15, 1.0),
        };
        colour.setFill();
        colour.setStroke();

        // The horn is FILLED: a stroked outline collapses into scribble at
        // this size, and the solid horn is the anchor that keeps the glyph
        // legible across the room (mark.rs's module doc).
        let horn = NSBezierPath::bezierPath();
        for (i, p) in m.horn.iter().enumerate() {
            let at = NSPoint::new(p.x, p.y);
            if i == 0 {
                horn.moveToPoint(at);
            } else {
                horn.lineToPoint(at);
            }
        }
        horn.closePath();
        horn.fill();

        // The sound arcs are STROKED with round caps, matching the logo's
        // arc treatment. Round caps also stop the arc ends reading as
        // clipped at 15pt.
        for wave in &m.waves {
            let path = NSBezierPath::bezierPath();
            for (i, p) in wave.iter().enumerate() {
                let at = NSPoint::new(p.x, p.y);
                if i == 0 {
                    path.moveToPoint(at);
                } else {
                    path.lineToPoint(at);
                }
            }
            path.setLineWidth(mark::GLYPH_LINE_WIDTH);
            path.setLineCapStyle(NSLineCapStyle::Round);
            // Round joins: the arc is sampled as short segments, and mitre
            // spikes at the joints would fuzz the curve.
            path.setLineJoinStyle(NSLineJoinStyle::Round);
            path.stroke();
        }

        #[allow(deprecated)]
        image.unlockFocus();
        // NOT a template: the colour above is the colour drawn. See the
        // comment in `set_glyph` for why the template route was abandoned.
        image.setTemplate(false);
        image
    }

    fn build_menu(&self, items: &[MenuItem]) -> Retained<NSMenu> {
        let menu = NSMenu::new(self.mtm);
        // AppKit would otherwise ask the app delegate to validate every item
        // and disable the ones nobody claims. We own enablement explicitly.
        menu.setAutoenablesItems(false);
        for item in items {
            menu.addItem(&self.build_item(item));
        }
        menu
    }

    fn build_item(&self, item: &MenuItem) -> Retained<NSMenuItem> {
        match item {
            MenuItem::Separator => NSMenuItem::separatorItem(self.mtm),
            MenuItem::Label(text) => {
                let it = self.plain_item(text, None);
                // Disabled: it is information, not a control that does
                // nothing when clicked.
                it.setEnabled(false);
                it
            }
            MenuItem::Item {
                title,
                id,
                checked,
                enabled,
            } => {
                let it = self.plain_item(title, Some(sel!(aquaMenuAction:)));
                unsafe { it.setTarget(Some(&*self.target as &AnyObject)) };
                // isize, not u64: NSInteger is the wire format for tags, and
                // the ids are host-assigned small integers.
                it.setTag(tag_for(*id));
                it.setState(if *checked {
                    NSControlStateValueOn
                } else {
                    NSControlStateValueOff
                });
                it.setEnabled(*enabled);
                it
            }
            MenuItem::Submenu { title, items } => {
                let it = self.plain_item(title, None);
                it.setSubmenu(Some(&self.build_menu(items)));
                it
            }
        }
    }

    fn plain_item(&self, title: &str, action: Option<Sel>) -> Retained<NSMenuItem> {
        unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(
                NSMenuItem::alloc(self.mtm),
                &NSString::from_str(title),
                action,
                // No key equivalents: a global shortcut on an accessory app's
                // status menu would only work while the menu is open, which
                // reads as a broken shortcut.
                &NSString::from_str(""),
            )
        }
    }
}

/// The integer type AppKit stores menu tags in.
type MenuIdRepr = isize;

/// `MenuId` -> tag conversion, saturating rather than wrapping so an absurd
/// id can never collide with a real one.
fn tag_for(id: MenuId) -> MenuIdRepr {
    MenuIdRepr::try_from(id.0).unwrap_or(MenuIdRepr::MAX)
}

impl Drop for MacStatusItem {
    fn drop(&mut self) {
        // Explicit removal: without it the item can linger in the menu bar
        // until the process actually exits, which looks like a zombie
        // daemon during a graceful shutdown.
        NSStatusBar::systemStatusBar().removeStatusItem(&self.item);
    }
}
