use std::io::{Read, Write};
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result};

/// Open the user's preferred editor on `path`. Returns an error if neither
/// `VISUAL` nor `EDITOR` is set, if the editor fails to launch, or if it
/// exits with a non-zero status.
///
/// Used by both the mood/body editor (passing a tempfile path) and the
/// `im :config` command (passing the live config path). The body editor
/// reads back the file after save; `:config` just hands control to the editor.
pub fn open_editor_at(path: &Path) -> Result<()> {
    let editor = std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .map_err(|_| anyhow::anyhow!("Neither VISUAL nor EDITOR environment variable is set. Set one to use the editor (..) feature."))?;

    let status = Command::new(&editor)
        .arg(path)
        .status()
        .with_context(|| format!("Failed to open editor: {}", editor))?;

    if !status.success() {
        anyhow::bail!("Editor exited with non-zero status");
    }
    Ok(())
}

/// Open the user's preferred editor on a temporary file pre-filled with
/// `initial`, and return the edited content (trimmed).
///
/// Unlike `open_editor_for_body`, there is no comment header to strip — the
/// temp file starts with the existing content so the user edits in place.
/// Used by the TUI Edit action for task/mood bodies and tracker values.
pub fn open_editor_on_text(initial: &str) -> Result<String> {
    let mut temp_file =
        tempfile::NamedTempFile::new().context("Failed to create temporary file")?;

    write!(temp_file, "{}", initial).context("Failed to write to temporary file")?;
    temp_file.flush()?;

    open_editor_at(temp_file.path())?;

    let mut content = String::new();
    std::fs::File::open(temp_file.path())
        .context("Failed to reopen temporary file")?
        .read_to_string(&mut content)
        .context("Failed to read from temporary file")?;

    Ok(content.trim().to_string())
}

/// Open the user's preferred editor on a temporary file and return the body content.
/// When `show_hint` is true the first line is a `# additional notes below`
/// comment header that gets stripped from the result (config `[editor] hint`);
/// when false the file starts empty and the result is returned verbatim
/// (trimmed). Returns an empty string if the editor produced no meaningful
/// content.
pub fn open_editor_for_body(show_hint: bool) -> Result<String> {
    let mut temp_file =
        tempfile::NamedTempFile::new().context("Failed to create temporary file")?;

    if show_hint {
        writeln!(temp_file, "# additional notes below")
            .context("Failed to write to temporary file")?;
    }
    temp_file.flush()?;

    open_editor_at(temp_file.path())?;

    let mut content = String::new();
    std::fs::File::open(temp_file.path())
        .context("Failed to reopen temporary file")?
        .read_to_string(&mut content)
        .context("Failed to read from temporary file")?;

    let body = if show_hint {
        content
            .lines()
            .skip(1)
            .collect::<Vec<_>>()
            .join("\n")
            .trim()
            .to_string()
    } else {
        content.trim().to_string()
    };

    Ok(body)
}
