use std::env;
use std::fs::File;
use std::io::copy;
use std::path::{Path, PathBuf};

/// The nomic-embed-text-v1.5 ONNX model bundled into the binary. The quantized
/// model is vendored at `assets/model/embed.onnx` (tracked with git-lfs) so CI
/// never needs Python: `model/quantize_qdq.py` downloads the FP32 model and
/// rewrites it into an INT8 QDQ graph with dynamic input shapes (no onnxsim
/// shape-folding — ONNX Runtime handles dynamic sequence lengths natively).
/// The quantized model is ~131 MB of INT8. `src/embed.rs` embeds it directly
/// via `include_bytes!`; anything below the floor is a truncated download,
/// stale file, or corrupt artifact and is regenerated.
///
/// Which model is embedded is driven by the `EMBED_MODEL` environment variable
/// (default `nomic`; see `model/quantize_qdq.py`'s `EMBED_ALIASES`). The choice is
/// recorded in `assets/model/.embed_model_stamp`; a mismatch between the stamp and
/// `EMBED_MODEL` regenerates `embed.onnx`, `tokenizer.json`, and
/// `saliency_adaptor.onnx` via `pixi run --manifest-path model/pixi.toml python`
/// (falling back to a bare `python3` when pixi is unavailable).
const EMBED_MODEL_NAME: &str = "embed";
const DEFAULT_EMBED_MODEL: &str = "nomic";
const MODEL_MIN_BYTES: u64 = 100_000_000;

/// Raw FP32 cache filename for an embedding alias, matching the `safe_name`
/// `model/quantize_qdq.py` computes for it (nomic -> `nomic_embed_v1_5`,
/// bge -> `bge_small`), so the `target/cache` download is reused across runs.
fn raw_fp32_name(alias: &str) -> String {
    let safe = match alias {
        "nomic" => "nomic_embed_v1_5",
        "bge" => "bge_small",
        other => other,
    };
    format!("embed_{safe}_fp32.onnx")
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=assets/help.txt");
    println!("cargo:rerun-if-env-changed=EMBED_MODEL");

    #[cfg(target_os = "linux")]
    println!("cargo:rustc-link-lib=dylib=stdc++");

    #[cfg(all(target_os = "windows", feature = "load-dynamic"))]
    {
        println!("cargo:rustc-link-lib=ucrt");
        println!("cargo:rustc-link-lib=oldnames");
    }

    update_readme_usage();

    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    generate_default_pairs(&out_dir);
    let cache = cache_dir();

    let embed_model = env::var("EMBED_MODEL").unwrap_or_else(|_| DEFAULT_EMBED_MODEL.to_string());

    let model_filename = format!("{EMBED_MODEL_NAME}.onnx");
    // Raw FP32 cache uses the safe alias name to avoid colliding with other model caches
    let raw_filename = raw_fp32_name(&embed_model);

    let vendored_model = manifest_dir
        .join("assets")
        .join("model")
        .join(&model_filename);
    println!("cargo:rerun-if-changed={}", vendored_model.display());

    // `.embed_model_stamp` records which embedding model the vendored
    // embed.onnx / tokenizer.json / saliency_adaptor.onnx were generated for.
    // It is committed to the repo so a clean checkout skips generation.
    let embed_stamp_file = manifest_dir.join("assets/model/.embed_model_stamp");
    let current_embed_stamp = std::fs::read_to_string(&embed_stamp_file).unwrap_or_default();
    let embed_mismatch = current_embed_stamp.trim() != embed_model.trim();
    let needs_embed_gen =
        embed_mismatch || file_len(&vendored_model).is_none_or(|len| len < MODEL_MIN_BYTES);

    if needs_embed_gen {
        // A vendored model built for a different embedding model must not be reused.
        if vendored_model.exists() && embed_mismatch {
            let _ = std::fs::remove_file(&vendored_model);
        }

        let script = manifest_dir.join("model/quantize_qdq.py");
        let pixi_toml = manifest_dir.join("model/pixi.toml");
        let raw_path = cache.join(&raw_filename);
        let generated = cache.join(&model_filename);

        let status = std::process::Command::new("pixi")
            .arg("run")
            .arg("--manifest-path")
            .arg(&pixi_toml)
            .arg("python")
            .arg(&script)
            .arg("embedding")
            .arg(&embed_model)
            .arg(&raw_path)
            .arg(&generated)
            .status()
            .or_else(|_| {
                std::process::Command::new("python3")
                    .arg(&script)
                    .arg("embedding")
                    .arg(&embed_model)
                    .arg(&raw_path)
                    .arg(&generated)
                    .status()
            })
            .expect("Failed to execute model/quantize_qdq.py (pixi run or python3)");

        if !status.success() || file_len(&generated).is_none_or(|l| l < MODEL_MIN_BYTES) {
            panic!(
                "Quantization script failed to generate quantized model at {}",
                generated.display()
            );
        }
        let _ = std::fs::copy(&generated, &vendored_model);
        let _ = std::fs::write(&embed_stamp_file, embed_model.trim());
    }

    // The model, tokenizer, and saliency adaptor are bundled as
    // `assets/model/{embed.onnx, tokenizer.json, saliency_adaptor.onnx}`
    // (refreshed by the quantize script whenever the embedding model is
    // regenerated) and are compiled into the binary via `include_bytes!` in
    // src/embed.rs directly from the manifest dir — nothing is downloaded or
    // copied at build time, and no ONNX codegen runs (ort loads the ONNX at
    // runtime).
}

/// `CARGO_MANIFEST_DIR/target/cache` — shared download cache for build-time
/// artifacts. The quantization script downloads the raw FP32 model here
/// (`embed_<safe_name>_fp32.onnx`) and writes the INT8 QDQ graph here
/// (`embed.onnx`) before it is copied into `assets/model/`.
fn cache_dir() -> PathBuf {
    PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap())
        .join("target")
        .join("cache")
}

/// Emit `default_axes.rs` into `OUT_DIR` — the bundled config's
/// `[[moods.axes]]` section compiled into a Rust fn, so startup never
/// re-parses TOML for the default axes. Debug builds read `assets/dev.toml`,
/// release builds `assets/config.toml`, mirroring the `cfg(debug_assertions)`
/// gating of `Config::DEFAULT_CONFIG`.
/// Emit `default_pairs.rs` into `OUT_DIR` — the bundled config's
/// `[[moods.pairs]]` section compiled into a Rust fn, so startup never
/// re-parses TOML for the default pairs. Debug builds read `assets/dev.toml`,
/// release builds `assets/config.toml`, mirroring the `cfg(debug_assertions)`
/// gating of `Config::DEFAULT_CONFIG`.
fn generate_default_pairs(out_dir: &Path) {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let profile = env::var("PROFILE").unwrap_or_default();
    let config_name = if profile == "release" {
        "assets/config.toml"
    } else {
        "assets/dev.toml"
    };
    let config_path = manifest_dir.join(config_name);
    println!("cargo:rerun-if-changed={}", config_path.display());

    let src = std::fs::read_to_string(&config_path)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", config_path.display()));
    let doc: toml::Value = toml::from_str(&src)
        .unwrap_or_else(|e| panic!("failed to parse {}: {e}", config_path.display()));

    let pairs = doc
        .get("moods")
        .and_then(|m| m.get("pairs"))
        .and_then(|a| a.as_array())
        .unwrap_or_else(|| panic!("[{config_name}] has no [[moods.pairs]] section"));

    let mut out = String::from(
        "/// Default mood pairs, compiled in from the bundled config's `[[moods.pairs]]`\n\
         /// section by `build.rs` (debug: `assets/dev.toml`, release: `assets/config.toml`,\n\
         /// mirroring `Config::DEFAULT_CONFIG`).\n\
         pub(crate) fn default_pairs() -> Vec<MoodEndpoint> {\n    vec![\n",
    );
    for pair in pairs {
        out.push_str(&format!(
            "        MoodEndpoint {{ mood: {mood:?}.to_string(), color: {} }},\n",
            color_expr(
                pair.get("color")
                    .and_then(|c| c.as_str())
                    .unwrap_or_else(|| panic!("pair missing string `color`"))
            ),
            mood = pair
                .get("mood")
                .and_then(|m| m.as_str())
                .unwrap_or_else(|| panic!("pair missing string `mood`"))
        ));
    }
    out.push_str("    ]\n}\n");

    std::fs::write(out_dir.join("default_pairs.rs"), out)
        .expect("failed to write default_pairs.rs");
}

/// Map a bundled-config color string to a `crossterm::style::Color` literal.
/// Accepts `#RRGGBB` hex, `rgb_(r,g,b)` tuples, and the named crossterm
/// colors (the same set its serde feature accepts).
fn color_expr(color: &str) -> String {
    let color = color.trim();
    if let Some(hex) = color.strip_prefix('#') {
        let v =
            u32::from_str_radix(hex, 16).unwrap_or_else(|_| panic!("invalid hex color {color:?}"));
        return format!(
            "Color::Rgb {{ r: {}, g: {}, b: {} }}",
            (v >> 16) & 0xFF,
            (v >> 8) & 0xFF,
            v & 0xFF,
        );
    }
    if let Some(inner) = color.strip_prefix("rgb_") {
        let inner = inner.trim_start_matches('(').trim_end_matches(')');
        let parts: Vec<&str> = inner.split(',').collect();
        let parse = |s: &str| s.trim().parse::<u8>().ok();
        if parts.len() == 3 {
            if let (Some(r), Some(g), Some(b)) = (parse(parts[0]), parse(parts[1]), parse(parts[2]))
            {
                return format!("Color::Rgb {{ r: {r}, g: {g}, b: {b} }}");
            }
        }
        panic!("invalid rgb_(r,g,b) color {color:?}");
    }
    let variant = match color {
        "reset" => "Reset",
        "black" => "Black",
        "grey" | "dark_grey" => "DarkGrey",
        "red" => "Red",
        "dark_red" => "DarkRed",
        "green" => "Green",
        "dark_green" => "DarkGreen",
        "yellow" => "Yellow",
        "dark_yellow" => "DarkYellow",
        "blue" => "Blue",
        "dark_blue" => "DarkBlue",
        "magenta" => "Magenta",
        "dark_magenta" => "DarkMagenta",
        "cyan" => "Cyan",
        "dark_cyan" => "DarkCyan",
        "white" => "White",
        _ => panic!(
            "unsupported color {color:?} (use #RRGGBB, rgb_(r,g,b), or a named crossterm color)"
        ),
    };
    format!("Color::{variant}")
}

fn file_len(path: &Path) -> Option<u64> {
    path.metadata().ok().map(|m| m.len())
}

/// The tokenizer is bundled as `assets/model/tokenizer.json` (refreshed by
/// `model/quantize_qdq.py`), so nothing is downloaded at build time anymore.
/// `download` is kept for manual/offline rebuilds of a non-default embedding
/// model.
#[allow(dead_code)]
const TOKENIZER_URL: &str =
    "https://huggingface.co/nomic-ai/nomic-embed-text-v1.5/resolve/main/tokenizer.json";

/// Download `url` to `dest` (creating parent dirs), ignoring failures: a
/// failed download leaves the file absent, and the caller falls back to
/// whatever is already on disk.
#[allow(dead_code)]
fn download(url: &str, dest: &Path) {
    if let Some(parent) = dest.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let Ok(mut resp) = ureq::get(url).call() else {
        return;
    };
    let Ok(mut file) = File::create(dest) else {
        return;
    };
    let _ = copy(&mut resp.body_mut().as_reader(), &mut file);
}

fn update_readme_usage() {
    let help_path = Path::new("assets/help.txt");
    let readme_path = Path::new("README.md");

    if !help_path.exists() || !readme_path.exists() {
        return;
    }

    let help_content = match std::fs::read_to_string(help_path) {
        Ok(c) => c,
        Err(_) => return,
    };

    let readme_content = match std::fs::read_to_string(readme_path) {
        Ok(c) => c,
        Err(_) => return,
    };

    let start_marker = "<!-- HELP_START -->";
    let end_marker = "<!-- HELP_END -->";

    if let (Some(start_idx), Some(end_idx)) = (
        readme_content.find(start_marker),
        readme_content.find(end_marker),
    ) {
        let before = &readme_content[..start_idx + start_marker.len()];
        let after = &readme_content[end_idx..];
        let new_block = format!("\n```\n{}\n```\n", help_content.trim());
        let updated_readme = format!("{}{}{}", before, new_block, after);

        if updated_readme != readme_content {
            let _ = std::fs::write(readme_path, updated_readme);
        }
    }
}
