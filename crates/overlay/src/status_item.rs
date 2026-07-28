//! The macOS menu-bar presence: an `NSStatusItem` whose glyph tracks the
//! product state and whose menu is rebuilt from a [`MenuModel`].
//!
//! Why this exists at all: `Aqua.app` sets `LSUIElement`, so it has no Dock
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
use objc2::{define_class, msg_send, sel, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSColor, NSControlStateValueOff, NSControlStateValueOn, NSFontWeightRegular, NSImage,
    NSImageSymbolConfiguration, NSImageSymbolScale, NSMenu, NSMenuItem, NSStatusBar, NSStatusItem,
    NSVariableStatusItemLength,
};
use objc2_foundation::{NSObject, NSString};

use crate::menu::{
    fallback_glyph, glyph_tint, sf_symbol, MenuId, MenuItem, MenuModel, GLYPH_POINT_SIZE,
};
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
    #[name = "AquaStatusTarget"]
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
        })
    }

    /// Push a model to the menu bar. Cheap and idempotent: identical models
    /// are ignored, so the host can call this every frame.
    pub fn apply(&mut self, model: &MenuModel) {
        if self.applied.as_ref() == Some(model) {
            return;
        }
        self.set_glyph(model);
        let menu = self.build_menu(&model.items);
        self.item.setMenu(Some(&menu));
        self.applied = Some(model.clone());
    }

    /// Take everything the user clicked since the last call. The host maps
    /// ids to actions; this crate never does.
    pub fn drain_clicks(&self) -> Vec<MenuId> {
        std::mem::take(&mut *self.target.ivars().borrow_mut())
    }

    fn set_glyph(&self, model: &MenuModel) {
        let Some(button) = self.item.button(self.mtm) else {
            // No button means no visible item; there is nothing we can do
            // about it here, and the menu is still attached to the item.
            return;
        };
        let desc = NSString::from_str(&model.tooltip);
        let name = NSString::from_str(sf_symbol(model.state));
        let image = NSImage::imageWithSystemSymbolName_accessibilityDescription(&name, Some(&desc));
        match image {
            Some(img) => {
                // A point-size configuration rather than a resized bitmap:
                // menu bar height varies with the notch, with HiDPI, and with
                // the user's menu bar size setting.
                // SAFETY: reading an AppKit weight constant; it is `static`
                // only because it is exported from the framework.
                let weight = unsafe { NSFontWeightRegular };
                let config = NSImageSymbolConfiguration::configurationWithPointSize_weight_scale(
                    GLYPH_POINT_SIZE,
                    weight,
                    NSImageSymbolScale::Medium,
                );
                let img = img.imageWithSymbolConfiguration(&config).unwrap_or(img);
                let tint = glyph_tint(model.state);
                // Template only when untinted: a template image is recolored
                // by the system for light/dark menu bars and for the
                // highlighted state, which is exactly what a quiet monochrome
                // glyph wants. A tinted glyph must keep its own colour, so it
                // opts out and supplies the tint explicitly.
                img.setTemplate(tint.is_none());
                button.setImage(Some(&img));
                button.setContentTintColor(tint.map(ns_color).as_deref());
                button.setTitle(&NSString::from_str(""));
            }
            None => {
                // Symbol lookup failed (renamed on a future OS, stripped
                // install). A text glyph is ugly; an empty menu bar item is
                // a bug, so ugly wins.
                button.setImage(None);
                button.setContentTintColor(None);
                button.setTitle(&NSString::from_str(fallback_glyph(model.state)));
            }
        }
        button.setToolTip(Some(&NSString::from_str(&model.tooltip)));
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
