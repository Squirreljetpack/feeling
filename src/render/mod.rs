mod preview;
mod utils;
pub use preview::*;
pub use utils::*;

pub mod system;
pub mod tasks;
pub mod today;

use std::io::Write;

use anyhow::{Context, Result};
use ratatui::layout::Rect;
use ratatui::Frame;
use tokio::sync::mpsc;

use crate::action::Action;
use crate::binds::default_binds;
use crate::event_loop::EventLoop;
use crate::message::{ControlEvent, RenderEvent};
use crate::tui::Tui;

/// Shared interface for the TUI views (`TasksApp` and `TodayApp`).
///
/// A view implements [`Render::render`] (drawing) and
/// [`Render::handle_action`] (state updates); the default [`Render::run`]
/// drives the full TUI lifecycle around them — terminal setup, the event
/// loop, redrawing on every event, and shutdown — so the two apps share
/// one loop implementation.
#[allow(async_fn_in_trait)]
pub trait Render {
    /// Draw the current state to the terminal.
    fn render(&self, f: &mut Frame);

    /// Handle a single action, whether from a keypress or injected by the
    /// view itself (e.g. `Accept` in a DeleteConfirm modal re-sends
    /// `Delete(true)` through `tx`).
    ///
    /// `controller` pauses/resumes the event loop around external processes
    /// (the Edit editor); `tx` injects follow-up actions into the event
    /// stream; `rx` is the render-event receiver (also used to wait for the
    /// event loop's `Ack` after pausing/resuming).
    async fn handle_action(
        &mut self,
        tui: &mut Tui<impl Write>,
        controller: &mpsc::UnboundedSender<ControlEvent>,
        tx: &mpsc::UnboundedSender<RenderEvent>,
        rx: &mut mpsc::UnboundedReceiver<RenderEvent>,
        action: Action,
    );

    /// Whether the view has handled `Action::Quit` and the loop should exit.
    fn should_quit(&self) -> bool;

    /// Run the full TUI lifecycle: enter fullscreen, spawn the event loop,
    /// redraw on every event, dispatch actions, exit on quit.
    async fn run(&mut self) -> Result<()> {
        let mut tui: Tui<Box<dyn Write + Send>> = Tui::new().context("Failed to create TUI")?;
        tui.enter().context("Failed to enter fullscreen")?;

        // Render-event channel: the event loop emits input-derived events;
        // the view injects follow-up actions through the same sender.
        let (controller_tx, controller_rx) = mpsc::unbounded_channel();
        let (tx, mut rx) = mpsc::unbounded_channel();
        let event_loop = EventLoop::new(default_binds(), tx.clone(), controller_rx);
        tokio::spawn(async move {
            let _ = event_loop.run().await;
        });

        while !self.should_quit() {
            tui.terminal.draw(|f| self.render(f))?;
            match rx.recv().await {
                Some(RenderEvent::Action(action)) => {
                    self.handle_action(&mut tui, &controller_tx, &tx, &mut rx, action)
                        .await;
                }
                Some(RenderEvent::Resize { width, height }) => {
                    tui.resize(Rect::new(0, 0, width, height));
                    // ratatui picks up the new size on the next draw.
                }
                None => break,
            }
        }
        tui.exit();
        Ok(())
    }
}
