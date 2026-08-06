use crokey::{key, KeyCombination};
use std::collections::HashMap;

use crate::action::Action;

/// A key combination (keycode + modifiers) that can be bound to an action.
pub type Trigger = KeyCombination;

/// Key combination → action bindings.
pub type BindMap = HashMap<Trigger, Action>;

/// Default key bindings. Bound triggers are looked up exactly (keycode +
/// modifiers); anything else falls through to [`map_key`]'s `Input` fallback.
///
/// This is intentionally view-agnostic: every key maps to one Action, and
/// each render loop ignores the variants it doesn't care about.
pub fn default_binds() -> BindMap {
    let mut binds = BindMap::new();
    // Quit
    binds.insert(key!(q), Action::Quit);
    binds.insert(key!(esc), Action::Quit);
    // Navigation
    binds.insert(key!(up), Action::Up);
    binds.insert(key!(k), Action::Up);
    binds.insert(key!(down), Action::Down);
    binds.insert(key!(j), Action::Down);
    binds.insert(key!(left), Action::Left);
    binds.insert(key!(h), Action::Left);
    binds.insert(key!(right), Action::Right);
    binds.insert(key!(l), Action::Right);
    // Mode / horizon cycle (each render loop interprets this differently)
    binds.insert(key!(tab), Action::CycleMode);
    // Sort toggle
    binds.insert(key!(ctrl - s), Action::ToggleSort);
    // Show-variant cycle (All → A → B → All; tasks app + today app)
    binds.insert(key!(ctrl - d), Action::CycleShow);
    // Primary actions
    binds.insert(key!(enter), Action::Accept);
    binds.insert(key!(delete), Action::Delete(false));
    binds.insert(key!(backspace), Action::Delete(false));
    binds.insert(key!(ctrl - e), Action::Edit);
    binds.insert(key!(ctrl - r), Action::Refresh);
    binds
}
