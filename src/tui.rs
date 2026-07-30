use anyhow::Result;
use crossterm::{
    event::{DisableMouseCapture, EnableMouseCapture},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use log::debug;
use ratatui::{backend::CrosstermBackend, Terminal, TerminalOptions, Viewport};
use std::io::Write;

// ---------- IO Stream Abstraction ----------

#[derive(Debug, Clone, Default)]
pub enum IoStream {
    Stdout,
    #[default]
    BufferedStderr,
}

impl IoStream {
    pub fn to_stream(&self) -> Box<dyn Write + Send> {
        match self {
            IoStream::Stdout => Box::new(std::io::stdout()),
            IoStream::BufferedStderr => Box::new(std::io::LineWriter::new(std::io::stderr())),
        }
    }
}

// ---------- Terminal Wrapper ----------

pub struct Tui<W: Write> {
    pub terminal: Terminal<CrosstermBackend<W>>,
    pub area: ratatui::layout::Rect,
    in_execute: bool,
}

// Concrete constructor for the common boxed-writer case
impl Tui<Box<dyn Write + Send>> {
    pub fn new() -> Result<Self> {
        Self::new_with_writer(IoStream::BufferedStderr.to_stream())
    }
}

impl<W: Write> Tui<W> {
    /// Create a new Tui with the given writer, always fullscreen.
    pub fn new_with_writer(writer: W) -> Result<Self> {
        enable_raw_mode()?;

        let (width, height) = crossterm::terminal::size().unwrap_or((80, 24));
        let area = ratatui::layout::Rect::new(0, 0, width, height);

        let backend = CrosstermBackend::new(writer);
        let terminal = Terminal::with_options(
            backend,
            TerminalOptions {
                viewport: Viewport::Fullscreen,
            },
        )?;

        debug!("TUI created: {width}x{height}");
        Ok(Self {
            terminal,
            area,
            in_execute: false,
        })
    }

    /// Enter fullscreen: raw mode + alternate screen + mouse capture.
    pub fn enter(&mut self) -> Result<()> {
        enable_raw_mode()?;
        execute!(
            self.terminal.backend_mut(),
            EnterAlternateScreen,
            EnableMouseCapture,
        )?;
        self.terminal.clear()?;
        debug!("TUI entered (fullscreen)");
        Ok(())
    }

    /// Exit fullscreen, restoring the terminal.
    pub fn exit(&mut self) {
        if self.in_execute {
            debug!("Skipping teardown after enter_execute");
            return;
        }
        let _ = execute!(
            self.terminal.backend_mut(),
            LeaveAlternateScreen,
            DisableMouseCapture,
        );
        let _ = self.terminal.show_cursor();
        let _ = disable_raw_mode();
        debug!("TUI exited");
    }

    /// Exit fullscreen before spawning an external command.
    pub fn enter_execute(&mut self) {
        self.exit();
        self.in_execute = true;
    }

    /// Re-enter fullscreen after an external command finishes.
    pub fn return_execute(&mut self) -> Result<()> {
        self.enter()?;
        self.resize(self.area);
        self.in_execute = false;
        Ok(())
    }

    pub fn resize(&mut self, area: ratatui::layout::Rect) {
        let _ = self.terminal.resize(area);
        self.area = area;
    }
}

impl<W: Write> Drop for Tui<W> {
    fn drop(&mut self) {
        self.exit();
    }
}
