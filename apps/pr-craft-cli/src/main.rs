//! `pr-craft-cli <command> [params-json]`: runs one engine command and prints its JSON result. `share.save` also runs
//! here: it needs the rasteriser, which the engine does not link.

use std::path::Path;
use std::process::ExitCode;

use pr_craft_core::share::ShareSize;
use serde_json::{Value, json};

/// `share.save`: `share.render`'s params plus `path`, which must end in `.png`, and optionally `"replace": true`. The path
/// is checked before anything is read from the forge. A file already at the path is kept and the command fails, unless
/// `replace` is true; the result says whether it replaced one. Writes the PNG and says what the image leaves out.
fn share_save(params: &Value) -> Result<Value, String> {
    let path = params.get("path").and_then(Value::as_str).ok_or("share.save needs a `path` ending in .png")?;
    let path = Path::new(path);
    if !path.extension().is_some_and(|e| e.eq_ignore_ascii_case("png")) {
        return Err(format!("{} does not end in .png; share images are saved as PNG files", path.display()));
    }
    let replace = params.get("replace").and_then(Value::as_bool).unwrap_or(false);
    // A cheap refusal before the forge is read; `write_new` below is what actually guarantees nothing is replaced.
    // `symlink_metadata`: a dangling link at the path counts as something there.
    if !replace && path.symlink_metadata().is_ok() {
        return Err(format!("{} already exists; pass \"replace\": true to replace it, or choose another path", path.display()));
    }
    let rendered = pr_craft_engine::run("share.render", params).map_err(|e| e.to_string())?;
    let svg = rendered.get("svg").and_then(Value::as_str).ok_or("share.render answered without an image")?;
    let size: ShareSize = serde_json::from_value(rendered["size"].clone()).map_err(|e| format!("share.render answered an unreadable size: {e}"))?;
    let png = pr_craft_share_render::png(svg, size)?;
    // `replaced` is what happened, not what was seen earlier: a file that appears while the image draws is reported.
    let (bytes, replaced) = match pr_craft_share_render::write_new(&png, path) {
        Ok(bytes) => (bytes, false),
        Err(pr_craft_share_render::NewError::Exists) if replace => (pr_craft_share_render::write_replacing(&png, path)?, true),
        Err(pr_craft_share_render::NewError::Exists) => {
            return Err(format!("{} appeared while the image was drawn; it was not replaced", path.display()));
        }
        Err(pr_craft_share_render::NewError::Failed(error)) => return Err(error),
    };
    let (width, height) = size.pixels();
    Ok(json!({
        "saved": path, "bytes": bytes, "width": width, "height": height, "replaced": replaced,
        "left_out": rendered["model"]["left_out"]
    }))
}

fn run(id: &str, params: &Value) -> Result<Value, String> {
    match id {
        "share.save" => share_save(params),
        _ => pr_craft_engine::run(id, params).map_err(|e| e.to_string()),
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(id) = args.first() else {
        eprintln!("usage: pr-craft-cli <command> [params-json]\ncommands: {}", pr_craft_engine::COMMANDS.join(", "));
        return ExitCode::from(2);
    };
    let params = match args.get(1).map(|raw| serde_json::from_str(raw)).transpose() {
        Ok(params) => params.unwrap_or(Value::Null),
        Err(error) => {
            eprintln!("params are not JSON: {error}");
            return ExitCode::from(2);
        }
    };
    match run(id, &params) {
        Ok(result) => {
            println!("{result:#}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}
