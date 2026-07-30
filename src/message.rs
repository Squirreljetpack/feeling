use crate::action::Action;

/// Events sent from the event loop to the render loop.
///
/// The event loop reads raw crossterm input and translates each key into an
/// `Action`, wrapping it — along with resize events — in a `RenderEvent` that
/// the render loop drains via `mpsc::UnboundedReceiver`. There are no
/// periodic ticks: the render loop only redraws when an event arrives.
#[derive(Debug, Clone)]
pub enum RenderEvent {
    /// An action emitted by the event loop in response to a key press.
    Action(Action),
    /// Terminal resize; the render loop should re-layout on receipt.
    Resize { width: u16, height: u16 },
}

/// Control directives sent from the render loop to the event loop.
///
/// The event loop acknowledges each directive with [`Action::Ack`] once it
/// has taken effect, so the render loop knows input capture has stopped /
/// restarted before (re)entering an external process (e.g. the Edit editor).
#[derive(Debug, Clone, Copy)]
pub enum ControlEvent {
    /// Stop reading input: an external process now owns the terminal. The
    /// event loop drops its event stream (so it doesn't capture keystrokes
    /// meant for that process) and replies `Action::Ack`.
    Pause,
    /// Resume reading input. The event loop recreates its event stream and
    /// replies `Action::Ack`.
    Resume,
}
