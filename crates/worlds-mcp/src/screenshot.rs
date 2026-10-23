//! `screenshot` without the editor: run the editor binary's headless
//! snapshot mode on the saved project (`docs/mcp.md`, "screenshot in
//! headless mode").

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use base64::Engine as _;
use serde_json::{Value, json};

use crate::engine::Engine;
use crate::tools::Screenshot;

/// The editor binary: `WORLDS_EDITOR_BIN`, else `worlds` next to this
/// executable.
pub fn editor_bin() -> PathBuf {
    if let Some(p) = std::env::var_os("WORLDS_EDITOR_BIN") {
        return PathBuf::from(p);
    }
    let exe = std::env::current_exe().unwrap_or_default();
    exe.parent()
        .map(|d| d.join(format!("worlds{}", std::env::consts::EXE_SUFFIX)))
        .unwrap_or_else(|| PathBuf::from("worlds"))
}

/// The arguments for one snapshot (everything but the binary).
pub(crate) fn snapshot_args(
    out: &std::path::Path,
    a: &Screenshot,
    project: &std::path::Path,
) -> Vec<String> {
    let mut args = vec![
        "--screenshot".to_string(),
        out.display().to_string(),
        "--snapshot".to_string(),
    ];
    if let Some(l) = &a.level {
        args.extend(["--level".to_string(), l.clone()]);
    }
    if let Some([x, y, w, h]) = a.world_rect {
        args.extend(["--world-rect".to_string(), format!("{x},{y},{w},{h}")]);
    }
    let [w, h] = a.size.unwrap_or([1024, 1024]);
    args.extend(["--max-size".to_string(), format!("{w}x{h}")]);
    if let Some(layers) = &a.layers {
        args.extend(["--layers".to_string(), layers.join(",")]);
    }
    args.push(project.display().to_string());
    args
}

/// Render with the editor binary; the result has the shape of
/// `view.snapshot`'s: `{ rev, png, width, height, scale }`.
pub(crate) fn headless(engine: &mut Engine, a: &Screenshot) -> Result<Value, String> {
    // The width in world pixels of what is drawn, for the scale.
    let target_w = match (&a.level, a.world_rect) {
        (Some(level), _) => {
            let d = engine.read("project.describe", json!({}))?;
            let found = d["levels"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|l| l["slug"] == level.as_str())
                .cloned();
            let Some(l) = found else {
                return Err(format!("there is no level {level:?}"));
            };
            l["size_px"][0].as_f64()
        }
        (_, Some(r)) => Some(r[2] as f64),
        _ => None,
    };
    static N: AtomicU64 = AtomicU64::new(0);
    let out = std::env::temp_dir().join(format!(
        "worlds-mcp-{}-{}.png",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let bin = editor_bin();
    let dir = engine.conn.dir().to_path_buf();
    let result = Command::new(&bin)
        .args(snapshot_args(&out, a, &dir))
        .output();
    let output = result.map_err(|e| {
        format!(
            "the screenshot needs the Worlds editor binary, and running {} failed: {e}. Build it (cargo build --release -p worlds-editor) next to worlds-mcp, or set WORLDS_EDITOR_BIN, or open the project in the editor",
            bin.display()
        )
    })?;
    if !output.status.success() {
        let _ = std::fs::remove_file(&out);
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "the headless screenshot failed: {} exited with {}: {}",
            bin.display(),
            output.status,
            stderr.trim()
        ));
    }
    let bytes = std::fs::read(&out).map_err(|e| {
        format!(
            "{} ran but wrote no image to {}: {e}",
            bin.display(),
            out.display()
        )
    })?;
    let _ = std::fs::remove_file(&out);
    let (width, height) = png_size(&bytes)
        .ok_or_else(|| format!("{} wrote something that isn't a PNG", bin.display()))?;
    let scale = target_w.filter(|w| *w > 0.0).map(|w| width as f64 / w);
    Ok(
        json!({ "rev": engine.rev, "png": base64::engine::general_purpose::STANDARD.encode(&bytes),
        "width": width, "height": height, "scale": scale }),
    )
}

/// Width and height from a PNG's IHDR.
fn png_size(b: &[u8]) -> Option<(u32, u32)> {
    if b.len() < 24 || &b[..8] != b"\x89PNG\r\n\x1a\n" {
        return None;
    }
    let w = u32::from_be_bytes(b[16..20].try_into().ok()?);
    let h = u32::from_be_bytes(b[20..24].try_into().ok()?);
    Some((w, h))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn args_follow_the_contract() {
        let a = Screenshot {
            level: Some("crevasse".into()),
            world_rect: None,
            layers: Some(vec!["collision".into(), "objects".into()]),
            size: None,
        };
        assert_eq!(
            snapshot_args(Path::new("/t/o.png"), &a, Path::new("/p/mini.worlds")),
            [
                "--screenshot",
                "/t/o.png",
                "--snapshot",
                "--level",
                "crevasse",
                "--max-size",
                "1024x1024",
                "--layers",
                "collision,objects",
                "/p/mini.worlds"
            ]
        );
    }
}
