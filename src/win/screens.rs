//! Display enumeration (§4.2): `EnumDisplayMonitors`, `rcMonitor`/`rcWork` in physical
//! pixels, `\\.\DISPLAYn` ids, effective DPI / 96 as the scale. Stub until P1.

use crate::screens::Screen;

/// Every display, primary first. Empty until P1 implements it.
pub fn displays() -> Vec<Screen> {
    Vec::new()
}

/// The same as `displays()`, callable from any thread.
pub fn current() -> Vec<Screen> {
    displays()
}
