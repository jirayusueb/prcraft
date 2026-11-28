//! Turns a share image's SVG (`pr_craft_core::share::svg`) into pixels and a PNG, through the craft facade.
//!
//! Text is set in the system's fonts: Geist and Geist Mono when installed, else Helvetica Neue, Arial or Menlo on a Mac.
//! No font file ships with PR Craft (AGENTS.md rule 25). With no usable font at all the render fails and says so,
//! rather than saving an image with its words missing.

use std::io::Write;
use std::path::Path;
use std::sync::{Arc, LazyLock};

use craft::image_codecs::{ChannelLayout, EncodeOptions, Format, Image};
use craft::svg::tree::draw::tiny_skia::Transform;
use craft::svg::tree::usvg::fontdb::Database;
use craft::svg::tree::{Limits, image_resolver, parse_text, usvg};
use pr_craft_core::share::ShareSize;

const SANS: [&str; 6] = ["Geist", "Helvetica Neue", "Helvetica", "Arial", "DejaVu Sans", "Liberation Sans"];
const MONO: [&str; 5] = ["Geist Mono", "Menlo", "Monaco", "DejaVu Sans Mono", "Courier New"];
/// Memory the rasteriser may hold; a share image is a few megabytes.
const BUDGET: u64 = 256 << 20;
/// usvg recurses over the tree; the thread gets the stack that takes.
const STACK: usize = 16 << 20;

/// A rendered share image.
pub struct Rendered {
    pub width: u32,
    pub height: u32,
    /// Opaque RGBA, row by row.
    pub rgba: Vec<u8>,
    pub png: Vec<u8>,
}

/// The system fonts, read once, and the family text falls back to.
struct Fonts {
    db: Arc<Database>,
    sans: String,
}

static FONTS: LazyLock<Result<Fonts, String>> = LazyLock::new(load_fonts);

fn fonts() -> Result<&'static Fonts, String> {
    FONTS.as_ref().map_err(Clone::clone)
}

fn load_fonts() -> Result<Fonts, String> {
    let mut db = Database::new();
    db.load_system_fonts();
    let has = |name: &str| db.faces().any(|face| face.families.iter().any(|(family, _)| family == name));
    let sans = SANS
        .iter()
        .find(|name| has(name))
        .map(|name| (*name).to_string())
        .or_else(|| db.faces().next().and_then(|f| f.families.first().map(|(n, _)| n.clone())));
    let mono = MONO.iter().find(|name| has(name)).map(|name| (*name).to_string());
    let sans = sans.ok_or("no fonts are installed on this system, so the image would have no words")?;
    db.set_sans_serif_family(sans.clone());
    if let Some(mono) = mono {
        db.set_monospace_family(mono);
    }
    Ok(Fonts { db: Arc::new(db), sans })
}

/// Draws `svg` at `size` and encodes it as PNG.
pub fn render(svg: &str, size: ShareSize) -> Result<Rendered, String> {
    let (width, height) = size.pixels();
    let fonts = fonts()?;
    std::thread::scope(|scope| {
        let job = std::thread::Builder::new().stack_size(STACK).spawn_scoped(scope, || draw(svg, width, height, fonts)).map_err(|e| e.to_string())?;
        job.join().unwrap_or_else(|_| Err("drawing the image stopped".into()))
    })
}

/// [`render`]'s PNG.
pub fn png(svg: &str, size: ShareSize) -> Result<Vec<u8>, String> {
    render(svg, size).map(|image| image.png)
}

/// Why [`write_new`] wrote nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NewError {
    /// Something is already at the path, a file or a symlink; nothing was touched.
    Exists,
    Failed(String),
}

fn check_png(path: &Path) -> Result<(), String> {
    if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("png")) {
        Ok(())
    } else {
        Err(format!("{} does not end in .png; share images are saved as PNG files", path.display()))
    }
}

/// Writes `png` to a path that must not exist yet: the file is created with `O_EXCL`, so nothing already there is
/// truncated and a symlink at the path is not followed. The path must end in `.png`. Returns the bytes written.
pub fn write_new(png: &[u8], path: &Path) -> Result<usize, NewError> {
    check_png(path).map_err(NewError::Failed)?;
    let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(path).map_err(|e| match e.kind() {
        std::io::ErrorKind::AlreadyExists => NewError::Exists,
        _ => NewError::Failed(format!("could not write {}: {e}", path.display())),
    })?;
    if let Err(error) = file.write_all(png) {
        // This call created the file, so the half-written PNG is its own to remove; leaving it would also make every
        // retry skip the name.
        drop(file);
        let _ = std::fs::remove_file(path);
        return Err(NewError::Failed(format!("could not write {}: {error}", path.display())));
    }
    Ok(png.len())
}

/// Writes `png` to `path`, replacing a file already there. The caller asked for that path and for the replacement. The
/// path must end in `.png`. Returns the bytes written.
pub fn write_replacing(png: &[u8], path: &Path) -> Result<usize, String> {
    check_png(path)?;
    std::fs::write(path, png).map_err(|e| format!("could not write {}: {e}", path.display()))?;
    Ok(png.len())
}

fn draw(svg: &str, width: u32, height: u32, fonts: &Fonts) -> Result<Rendered, String> {
    let limits = Limits { tree_depth: 48, depth: 96, ..Limits::default() };
    let options = usvg::Options {
        fontdb: fonts.db.clone(),
        font_family: fonts.sans.clone(),
        image_href_resolver: image_resolver(limits),
        ..usvg::Options::default()
    };
    let tree = parse_text(svg, &limits, &options).map_err(|e| e.to_string())?;
    let size = tree.size();
    if size.width().round() as u32 != width || size.height().round() as u32 != height {
        return Err(format!("the image is {}×{}, not {width}×{height}", size.width(), size.height()));
    }
    let pixmap = craft::svg::tree::draw::render(&tree, Transform::identity(), width, height, 0, BUDGET).map_err(|r| r.message)?;
    // The field is opaque, so premultiplied and straight pixels are the same.
    let rgba = pixmap.data().to_vec();
    let rgb: Vec<u8> = rgba.as_chunks::<4>().0.iter().flat_map(|p| [p[0], p[1], p[2]]).collect();
    let image = Image::from_u8(width, height, ChannelLayout::Rgb, rgb).map_err(|e| e.to_string())?;
    let png = craft::image_codecs::encode(&image, Format::Png, &EncodeOptions::default()).map_err(|e| e.to_string())?;
    Ok(Rendered { width, height, rgba, png })
}

#[cfg(test)]
mod tests {
    use craft::image_codecs::{DecodeOptions, decode_with};
    use pr_craft_core::share::{Big, Layer, LayerState, ShareKind, ShareModel, Stat, svg};

    use super::*;

    fn model(kind: ShareKind, headline: &str) -> ShareModel {
        let stack = kind == ShareKind::StackProgress;
        ShareModel {
            kind,
            eyebrow: format!("{} · Nov 14", kind.word()),
            headline: headline.into(),
            reference: "acme/api #482".into(),
            summary: Some("Failed deliveries now retry up to six times.".into()),
            big: Big { value: 14, caption: "files changed".into() },
            stats: if stack {
                Vec::new()
            } else {
                vec![
                    Stat { label: "Diff".into(), value: "+184  −37".into(), note: "6 files".into() },
                    Stat { label: "Files".into(), value: "6".into(), note: String::new() },
                ]
            },
            people: vec!["ada".into(), "grace".into()],
            layers: if stack {
                (1..=9)
                    .map(|n| Layer {
                        title: format!("Layer {n}"),
                        number: format!("#{n}"),
                        state: if n % 2 == 0 { LayerState::Draft } else { LayerState::InReview },
                        current: n == 3,
                    })
                    .collect()
            } else {
                Vec::new()
            },
            left_out: Vec::new(),
        }
    }

    /// Pixels that are not the night field, and pixels that are near white (type).
    fn counts(rgba: &[u8]) -> (usize, usize) {
        let (mut ink, mut white) = (0, 0);
        for p in rgba.as_chunks::<4>().0 {
            if p[..3] != [0x11, 0x11, 0x14] {
                ink += 1;
            }
            if p[..3].iter().all(|&c| c > 200) {
                white += 1;
            }
        }
        (ink, white)
    }

    #[test]
    fn every_kind_and_size_renders_a_png_of_its_size_with_words_in_it() {
        for kind in ShareKind::ALL {
            for size in ShareSize::ALL {
                let Ok(image) = render(&svg(&model(kind, "Retry idempotent webhooks with exponential backoff"), size), size) else {
                    panic!("{kind:?} {size:?} did not render")
                };
                let decoded = decode_with(&image.png, &DecodeOptions::default());
                let Ok(decoded) = decoded else { panic!("{kind:?} {size:?} is not a PNG") };
                assert_eq!((decoded.width(), decoded.height()), size.pixels(), "{kind:?} {size:?}");
                let (ink, white) = counts(&image.rgba);
                assert!(ink > 2_000, "{kind:?} {size:?} is nearly blank");
                assert!(white > 300, "{kind:?} {size:?} has no white type");
            }
        }
    }

    #[test]
    fn hostile_titles_render_inside_the_image() {
        let hostile = [
            "<script>alert(1)</script> & </text><rect/>",
            "مرحبا بالعالم ".repeat(40).as_str(),
            "🔥🚀 日本語のとても長いタイトル".repeat(30).as_str(),
            "x".repeat(20_000).as_str(),
        ]
        .map(str::to_string);
        for title in hostile {
            for size in ShareSize::ALL {
                let model = model(ShareKind::ReviewSummary, &title);
                assert!(render(&svg(&model, size), size).is_ok(), "{size:?}");
            }
        }
    }

    #[test]
    fn writing_refuses_a_path_that_is_not_a_png() {
        let path = Path::new("/tmp/pr-craft-share-not-a-png.txt");
        assert!(matches!(write_new(b"x", path), Err(NewError::Failed(e)) if e.contains(".png")));
        assert!(write_replacing(b"x", path).is_err());
        assert!(!path.exists());
    }

    #[test]
    fn write_new_never_touches_what_is_already_there_nor_follows_a_link() {
        let dir = std::env::temp_dir().join(format!("pr-craft-share-new-{}", std::process::id()));
        assert!(std::fs::create_dir_all(&dir).is_ok());
        let (file, link, target) = (dir.join("a.png"), dir.join("link.png"), dir.join("outside.txt"));
        assert_eq!(write_new(b"first", &file), Ok(5));
        assert_eq!(write_new(b"second", &file), Err(NewError::Exists));
        assert_eq!(std::fs::read(&file).ok().as_deref(), Some(b"first".as_slice()));
        // A dangling link: `exists()` is false for it, but nothing may be created through it.
        assert!(std::os::unix::fs::symlink(&target, &link).is_ok());
        assert_eq!(write_new(b"x", &link), Err(NewError::Exists));
        assert!(!target.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
