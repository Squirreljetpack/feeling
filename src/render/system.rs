use std::io::Write;

use tokio::sync::mpsc;

use crate::action::Action;
use crate::message::{ControlEvent, RenderEvent};
use crate::tui::Tui;

/// Open the user's editor on `initial` content, suspending the TUI first
/// (enter_execute → editor → return_execute). Returns the edited text, or
/// `None` if the user cancelled / no editor is configured.
///
/// `return_execute` is called unconditionally so the terminal is always
/// restored, even when the editor fails.
///
/// Before suspending, the event loop is paused ([`ControlEvent::Pause`],
/// waiting for its [`Action::Ack`]) so it doesn't capture keystrokes meant
/// for the editor; [`ControlEvent::Resume`] restores input capture
/// afterwards. If the event loop is already gone (send fails) there is
/// nothing to pause — the editor gets the terminal either way.
pub(crate) async fn edit_with_editor(
    tui: &mut Tui<impl Write>,
    controller: &mpsc::UnboundedSender<ControlEvent>,
    rx: &mut mpsc::UnboundedReceiver<RenderEvent>,
    initial: &str,
) -> Option<String> {
    pause_loop(controller, rx).await;
    tui.enter_execute();
    let result = crate::editor::open_editor_on_text(initial);
    if let Err(e) = tui.return_execute() {
        cba::ebog!("edit"; "return_execute: {e}");
    }
    resume_loop(controller, rx).await;
    match result {
        Ok(text) => Some(text),
        Err(e) => {
            cba::ebog!("edit"; "{e}");
            None
        }
    }
}

async fn pause_loop(
    controller: &mpsc::UnboundedSender<ControlEvent>,
    rx: &mut mpsc::UnboundedReceiver<RenderEvent>,
) {
    if controller.send(ControlEvent::Pause).is_ok() {
        wait_for_ack(rx).await;
    }
}

async fn resume_loop(
    controller: &mpsc::UnboundedSender<ControlEvent>,
    rx: &mut mpsc::UnboundedReceiver<RenderEvent>,
) {
    if controller.send(ControlEvent::Resume).is_ok() {
        wait_for_ack(rx).await;
    }
}

/// Wait until the event loop acknowledges the current control directive.
/// Stale events that arrive first (e.g. a resize) are discarded — a full
/// redraw happens on the next loop iteration anyway.
async fn wait_for_ack(rx: &mut mpsc::UnboundedReceiver<RenderEvent>) {
    while let Some(event) = rx.recv().await {
        if matches!(event, RenderEvent::Action(Action::Ack)) {
            return;
        }
    }
}
