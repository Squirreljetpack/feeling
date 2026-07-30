use std::env;
use std::fs::File;
use std::io::copy;
use std::path::{Path, PathBuf};

/// The bge-small-en-v1.5 ONNX model bundled into the binary. The quantized model
/// is vendored at `assets/model/bge_small.onnx` (tracked with git-lfs) so CI never
/// needs Python: `scripts/quantize_bge_qdq.py` downloads the FP32 model and
/// rewrites it into a static INT8 QDQ graph (each FP32 weight initializer becomes
/// an INT8 constant + per-tensor scale feeding a `DequantizeLinear` node) —
/// burn-onnx 0.21 compiles that into the binary via `LoadStrategy::Embedded`, so
/// the shipped weights are ~32 MB of INT8 rather than ~133 MB of FP32. Anything
/// below the floor is a truncated download, a stale smaller variant from a
/// previous build, or a corrupt file, and is regenerated.
const MODEL_NAME: &str = "bge_small";
const MODEL_URL: &str =
    "https://huggingface.co/xenova/bge-small-en-v1.5/resolve/main/onnx/model.onnx";
const TOKENIZER_URL: &str =
    "https://huggingface.co/xenova/bge-small-en-v1.5/resolve/main/tokenizer.json";
const MODEL_MIN_BYTES: u64 = 20_000_000;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=assets/help.txt");

    update_readme_usage();

    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    generate_default_pairs(&out_dir);
    let cache = cache_dir();

    let model_filename = format!("{MODEL_NAME}.onnx");
    let raw_filename = format!("{MODEL_NAME}_fp32.onnx");

    let vendored_model = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap())
        .join("assets")
        .join("model")
        .join(&model_filename);
    let onnx_path = out_dir.join(&model_filename);
    println!("cargo:rerun-if-changed={}", vendored_model.display());

    if file_len(&vendored_model).is_none_or(|len| len < MODEL_MIN_BYTES) {
        let script =
            PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap()).join("scripts/quantize_qdq.py");
        let raw_path = cache.join(&raw_filename);
        let generated = cache.join(&model_filename);

        let status = std::process::Command::new("python3")
            .arg(&script)
            .arg(MODEL_URL)
            .arg(&raw_path)
            .arg(&generated)
            .status()
            .expect("Failed to execute python3 scripts/quantize_qdq.py");

        if !status.success() || file_len(&generated).is_none_or(|l| l < MODEL_MIN_BYTES) {
            panic!(
                "Quantization script failed to generate quantized model at {}",
                generated.display()
            );
        }
        let _ = std::fs::copy(&generated, &vendored_model);
    }
    let _ = std::fs::copy(&vendored_model, &onnx_path);

    let tokenizer_cache = cache.join("tokenizer.json");
    let tokenizer_out = out_dir.join("tokenizer.json");
    println!("cargo:rerun-if-changed={}", tokenizer_cache.display());

    if !tokenizer_out.exists() {
        if !tokenizer_cache.exists() {
            download(TOKENIZER_URL, &tokenizer_cache);
        }
        let _ = std::fs::copy(&tokenizer_cache, &tokenizer_out);
    }

    if onnx_path.exists() {
        burn_onnx::ModelGen::new()
            .input(onnx_path.to_str().unwrap())
            .out_dir("model/")
            .load_strategy(burn_onnx::LoadStrategy::Embedded)
            .run_from_script();
    }

    let adaptor_filename = "saliency_adaptor.onnx";
    let vendored_adaptor = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap())
        .join("assets")
        .join("model")
        .join(adaptor_filename);
    let adaptor_onnx_path = out_dir.join(adaptor_filename);
    println!("cargo:rerun-if-changed={}", vendored_adaptor.display());

    if vendored_adaptor.exists() {
        let _ = std::fs::copy(&vendored_adaptor, &adaptor_onnx_path);
        burn_onnx::ModelGen::new()
            .input(adaptor_onnx_path.to_str().unwrap())
            .out_dir("model/")
            .load_strategy(burn_onnx::LoadStrategy::Embedded)
            .run_from_script();
    }
}

/// `CARGO_MANIFEST_DIR/target/cache` — shared download cache for build-time
/// artifacts (the tokenizer; the quantization script also drops the model here).
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

    std::fs::write(out_dir.join("default_pairs.rs"), out).expect("failed to write default_pairs.rs");
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

/// Download `url` to `dest` (creating parent dirs), ignoring failures: a
/// failed download leaves the file absent, and the caller falls back to
/// whatever is already on disk.
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
