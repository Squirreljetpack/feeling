use crokey::KeyCombination;
use crossterm::event::{Event as CtEvent, EventStream, KeyCode, KeyEventKind, KeyModifiers};
use futures::StreamExt;
use tokio::sync::mpsc;

use crate::ui::action::Action;
use crate::ui::bindings::BindMap;
use crate::ui::events::{ControlEvent, RenderEvent};

/// Event loop that listens to crossterm input, maps each key to an
/// [`Action`] via a [`BindMap`], and emits `RenderEvent`s through an mpsc
/// channel for the render loop to consume.
///
/// The event loop runs in a separate tokio task/thread. It never emits
/// periodic ticks: the render loop only redraws when an event arrives.
///
/// While an external process owns the terminal (e.g. the external editor
/// launched by `Edit`), the render loop sends [`ControlEvent::Pause`]; the
/// event loop drops its event stream — so the editor's keystrokes aren't
/// captured — and replies with [`Action::Ack`]. [`ControlEvent::Resume`]
/// recreates the stream (fresh, discarding any buffered events) and also
/// replies `Ack`.
pub struct EventLoop {
    tx: mpsc::UnboundedSender<RenderEvent>,
    ctrl_rx: mpsc::UnboundedReceiver<ControlEvent>,
    paused: bool,
    binds: BindMap,
    stream: Option<EventStream>,
}

impl EventLoop {
    /// Build an event loop that sends `RenderEvent`s through `tx` and
    /// receives [`ControlEvent`]s (Pause/Resume) through `ctrl_rx`. The
    /// caller owns both channel halves and keeps `tx` in the renderer so
    /// it can inject follow-up actions into its own event stream (no
    /// recursive action buffer needed).
    pub fn new(
        binds: BindMap,
        tx: mpsc::UnboundedSender<RenderEvent>,
        ctrl_rx: mpsc::UnboundedReceiver<ControlEvent>,
    ) -> Self {
        Self {
            tx,
            ctrl_rx,
            binds,
            paused: false,
            stream: None,
        }
    }

    /// Run until the render loop drops its receiver.
    pub async fn run(mut self) -> anyhow::Result<()> {
        self.stream = Some(EventStream::new());
        loop {
            // While paused (external process owns the terminal), don't read
            // input at all — just wait for a Resume control.
            while self.paused {
                match self.ctrl_rx.recv().await {
                    Some(ControlEvent::Resume) => {
                        self.paused = false;
                        self.stream = Some(EventStream::new());
                        if !self.send(RenderEvent::Action(Action::Ack)) {
                            return Ok(());
                        }
                    }
                    Some(ControlEvent::Pause) => {} // already paused
                    None => return Ok(()),
                }
            }

            let input = match self.stream.as_mut() {
                Some(stream) => stream.next(),
                None => continue,
            };

            tokio::select! {
                ctrl = self.ctrl_rx.recv() => {
                    match ctrl {
                        Some(ControlEvent::Pause) => {
                            self.paused = true;
                            // Drop the stream: its reader thread would steal
                            // keystrokes meant for the external process.
                            // (Recreated fresh on Resume.)
                            self.stream = None;
                            if !self.send(RenderEvent::Action(Action::Ack)) {
                                return Ok(());
                            }
                        }
                        Some(ControlEvent::Resume) => {} // not paused; nothing to do
                        // Controller dropped → render loop is exiting.
                        None => return Ok(()),
                    }
                }
                event = input => {
                    match event {
                        Some(Ok(CtEvent::Key(key))) if key.kind == KeyEventKind::Press => {
                            if let Some(action) = self.map_key(key.code, key.modifiers) {
                                if !self.send(RenderEvent::Action(action)) {
                                    return Ok(());
                                }
                            }
                        }
                        Some(Ok(CtEvent::Resize(width, height))) => {
                            if !self.send(RenderEvent::Resize { width, height }) {
                                return Ok(());
                            }
                        }
                        Some(Ok(_)) => {}
                        Some(Err(e)) => anyhow::bail!("failed to read crossterm event: {e}"),
                        None => return Ok(()),
                    }
                }
            }
        }
    }

    fn send(&self, event: RenderEvent) -> bool {
        self.tx.send(event).is_ok()
    }

    /// Map a single key press into an [`Action`], or `None` for keys we ignore
    /// (function keys, modifier-only presses, etc.).
    ///
    /// Bound keys are looked up in [`default_binds`]. Unbound characters without
    /// modifiers (or with plain shift) fall through to [`Action::Input`] — the
    /// render loop routes them to the active modal input, or ignores them when
    /// no modal is open. This mirrors matchmaker's `key_code_as_letter`.
    fn map_key(&self, code: KeyCode, modifiers: KeyModifiers) -> Option<Action> {
        let combo = KeyCombination::new(code, modifiers);
        if let Some(action) = self.binds.get(&combo) {
            return Some(action.clone());
        }
        match (code, modifiers) {
            (KeyCode::Char(c), KeyModifiers::NONE) => Some(Action::Input(c)),
            (KeyCode::Char(c), m) if m == KeyModifiers::SHIFT => {
                Some(Action::Input(c.to_ascii_uppercase()))
            }
            _ => None,
        }
    }
}
