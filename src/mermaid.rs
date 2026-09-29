//! Session-lifetime Mermaid rendering cache for assistant Markdown.
//!
//! Mermaid CLI work and terminal-protocol encoding run off the UI thread. The
//! cache is intentionally bounded; it lives for the lifetime of the loaded
//! interactive session/process.

use std::{
    collections::{HashMap, HashSet, VecDeque},
    path::{Path, PathBuf},
    process::Command,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicU64, Ordering},
    },
};

use ratatui::layout::Size;
use ratatui_image::{picker::Picker, sliced::SlicedProtocol};

use crate::app_event::{AppEvent, AppEventTx};
use sha2::{Digest, Sha256};

// Fixed render density: CSS-pixel font size and Puppeteer device scale factor
// do not depend on terminal geometry. Each PNG retains Mermaid's content bounds.
const MERMAID_FONT_SIZE_PX: u16 = 16;
const RENDER_SCALE: u8 = 2;
const OUTPUT_DOWNSCALE: u8 = 2;
const VIEWPORT_WIDTH: u16 = 1200;
const VIEWPORT_HEIGHT: u16 = 1200;
pub const FALLBACK_SLOT_MIN_HEIGHT: usize = 8;
const CACHE_LIMIT: usize = 64;
const CACHE_BYTES_LIMIT: usize = 32 * 1024 * 1024;

fn mermaid_config() -> String {
    format!(
        r##"{{"theme":"dark","themeVariables":{{"fontSize":"{MERMAID_FONT_SIZE_PX}px","lineColor":"#cbd5e1","textColor":"#f1f5f9","primaryTextColor":"#f1f5f9","secondaryTextColor":"#f1f5f9","tertiaryTextColor":"#f1f5f9"}}}}"##
    )
}
static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);
static RENDERER: OnceLock<Renderer> = OnceLock::new();

struct CachedImage {
    _png: Arc<[u8]>,
    protocol: Arc<SlicedProtocol>,
    size: Size,
}

struct Cache {
    images: HashMap<String, CachedImage>,
    image_bytes: usize,
    pending: HashSet<String>,
    failed: HashSet<String>,
    failed_order: VecDeque<String>,
    order: VecDeque<String>,
}

struct Renderer {
    picker: Picker,
    available: bool,
    cache: Mutex<Cache>,
    tx: AppEventTx,
}

/// Probe terminal graphics capabilities after entering the alternate screen,
/// before the event reader starts. Half-block rendering is the safe fallback.
pub fn initialize(tx: AppEventTx) {
    let available = mermaid_command()
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success());
    let picker = if available {
        match Picker::from_query_stdio() {
            Ok(picker) => picker,
            Err(error) => {
                log::debug!("Mermaid image terminal probe failed; using half-blocks: {error}");
                Picker::halfblocks()
            }
        }
    } else {
        log::debug!("Mermaid image rendering disabled: mmdc not found");
        Picker::halfblocks()
    };
    let _ = RENDERER.set(Renderer {
        picker,
        available,
        cache: Mutex::new(Cache {
            images: HashMap::new(),
            image_bytes: 0,
            pending: HashSet::new(),
            failed: HashSet::new(),
            failed_order: VecDeque::new(),
            order: VecDeque::new(),
        }),
        tx,
    });
}

/// Whether Mermaid CLI was available when the terminal session started.
pub fn available() -> bool {
    RENDERER.get().is_some_and(|renderer| renderer.available)
}

/// Natural terminal-cell dimensions for a cached diagram.
pub fn cached_size(source: &str) -> Option<Size> {
    let renderer = RENDERER.get()?;
    let key = key(source);
    renderer
        .cache
        .lock()
        .ok()?
        .images
        .get(&key)
        .map(|image| image.size)
}

/// Stable key for diagram source and render density.
pub fn key(source: &str) -> String {
    let mut hash = Sha256::new();
    hash.update(b"xi-mermaid-poc-v1\0");
    hash.update(MERMAID_FONT_SIZE_PX.to_le_bytes());
    hash.update([RENDER_SCALE]);
    hash.update([OUTPUT_DOWNSCALE]);
    hash.update(b"box-average-2x2-multistage");
    hash.update(VIEWPORT_WIDTH.to_le_bytes());
    hash.update(VIEWPORT_HEIGHT.to_le_bytes());
    hash.update(mermaid_config().as_bytes());
    if let Some(renderer) = RENDERER.get() {
        hash.update(renderer.picker.font_size().width.to_le_bytes());
        hash.update(renderer.picker.font_size().height.to_le_bytes());
        hash.update(format!("{:?}", renderer.picker.protocol_type()).as_bytes());
    }
    hash.update(source.as_bytes());
    format!("{:x}", hash.finalize())
}

/// Whether rendering failed for this diagram source.
pub fn is_failed(source: &str) -> bool {
    let Some(renderer) = RENDERER.get() else {
        return true;
    };
    renderer
        .cache
        .lock()
        .is_ok_and(|cache| cache.failed.contains(&key(source)))
}

/// Return a cached terminal image and its natural cell dimensions.
pub fn get(source: &str) -> Option<(Arc<SlicedProtocol>, Size)> {
    let renderer = RENDERER.get()?;
    let key = key(source);
    renderer
        .cache
        .lock()
        .ok()?
        .images
        .get(&key)
        .map(|image| (Arc::clone(&image.protocol), image.size))
}

/// Queue a diagram if it is neither cached nor already being rendered.
/// Returns true when a background render is pending.
pub fn request(source: &str) -> bool {
    let Some(renderer) = RENDERER.get() else {
        return false;
    };
    if !renderer.available {
        return false;
    }
    let key = key(source);
    {
        let Ok(mut cache) = renderer.cache.lock() else {
            return false;
        };
        if cache.images.contains_key(&key) || cache.failed.contains(&key) {
            return false;
        }
        if !cache.pending.insert(key.clone()) {
            return true;
        }
    }
    let Ok(handle) = tokio::runtime::Handle::try_current() else {
        if let Ok(mut cache) = renderer.cache.lock() {
            cache.pending.remove(&key);
        }
        return false;
    };
    let source = source.to_owned();
    let picker = renderer.picker.clone();
    let tx = renderer.tx.clone();
    // Reacquire the process-global cache in the worker; it outlives this task.
    handle.spawn_blocking(move || {
        let result = render_diagram(&source, &picker);
        let Some(renderer) = RENDERER.get() else {
            return;
        };
        let Ok(mut cache) = renderer.cache.lock() else {
            return;
        };
        cache.pending.remove(&key);
        match result {
            Ok(protocol) => {
                let png: Arc<[u8]> = Arc::from(protocol.png);
                if png.len() <= CACHE_BYTES_LIMIT {
                    while (cache.images.len() >= CACHE_LIMIT
                        || cache.image_bytes + png.len() > CACHE_BYTES_LIMIT)
                        && let Some(oldest) = cache.order.pop_front()
                    {
                        if let Some(removed) = cache.images.remove(&oldest) {
                            cache.image_bytes =
                                cache.image_bytes.saturating_sub(removed._png.len());
                        }
                    }
                    cache.image_bytes += png.len();
                    cache.order.push_back(key.clone());
                    cache.images.insert(
                        key,
                        CachedImage {
                            _png: png,
                            size: protocol.protocol.size(),
                            protocol: Arc::new(protocol.protocol),
                        },
                    );
                }
                drop(cache);
                let _ = tx.send(AppEvent::MermaidReady {
                    source: source.clone(),
                });
            }
            Err(error) => {
                log::debug!("Mermaid render failed; retaining code block fallback: {error:#}");
                if cache.failed.len() >= CACHE_LIMIT
                    && let Some(oldest) = cache.failed_order.pop_front()
                {
                    cache.failed.remove(&oldest);
                }
                cache.failed_order.push_back(key.clone());
                cache.failed.insert(key);
                drop(cache);
                let _ = tx.send(AppEvent::MermaidReady { source });
            }
        }
    });
    true
}

struct RenderedDiagram {
    png: Vec<u8>,
    protocol: SlicedProtocol,
}

fn render_diagram(source: &str, picker: &Picker) -> anyhow::Result<RenderedDiagram> {
    let config_contents = mermaid_config();
    let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
    let stem = format!("xi-mermaid-{}-{id}", std::process::id());
    let input = std::env::temp_dir().join(format!("{stem}.mmd"));
    let output = std::env::temp_dir().join(format!("{stem}.png"));
    let config = std::env::temp_dir().join(format!("{stem}.json"));
    let _cleanup = TempFiles(vec![input.clone(), output.clone(), config.clone()]);
    std::fs::write(&input, source)?;
    std::fs::write(&config, config_contents)?;
    let viewport_width = VIEWPORT_WIDTH.to_string();
    let viewport_height = VIEWPORT_HEIGHT.to_string();
    let render_scale = RENDER_SCALE.to_string();
    let mut command = mermaid_command();
    let status = command
        .args([
            "-i",
            path_arg(&input)?,
            "-o",
            path_arg(&output)?,
            "-b",
            "transparent",
            "-w",
            &viewport_width,
            "-H",
            &viewport_height,
            "-s",
            &render_scale,
            "-c",
            path_arg(&config)?,
            "-q",
        ])
        .output()?;
    anyhow::ensure!(
        status.status.success(),
        "mmdc exited with {}: {}",
        status.status,
        String::from_utf8_lossy(&status.stderr).trim()
    );
    let png = std::fs::read(&output)?;
    let high_resolution = image::load_from_memory(&png)?;
    // `mmdc --scale` sets Puppeteer's deviceScaleFactor: it increases raster
    // pixels but not the SVG's CSS-pixel dimensions. First average 2x2 samples
    // back to CSS pixels, then apply the separate output-size reduction.
    let image = downsample_render_scale(high_resolution);
    // Preserve each diagram's natural aspect and variable bounds after the
    // output reduction. Terminal rows/columns remain content-dependent.
    let protocol = SlicedProtocol::new(picker, image, None)?;
    Ok(RenderedDiagram { png, protocol })
}

fn downsample_render_scale(mut image: image::DynamicImage) -> image::DynamicImage {
    for factor in [RENDER_SCALE, OUTPUT_DOWNSCALE] {
        debug_assert_eq!(factor, 2, "box averaging stages must reduce by 2");
        image = average_2x2(image);
    }
    image
}

fn average_2x2(image: image::DynamicImage) -> image::DynamicImage {
    let source = image.to_rgba8();
    let width = source.width().div_ceil(2);
    let height = source.height().div_ceil(2);
    let mut output = image::RgbaImage::new(width, height);
    for y in 0..height {
        for x in 0..width {
            let x0 = x * 2;
            let y0 = y * 2;
            let x1 = (x0 + 1).min(source.width() - 1);
            let y1 = (y0 + 1).min(source.height() - 1);
            let samples = [
                source.get_pixel(x0, y0),
                source.get_pixel(x1, y0),
                source.get_pixel(x0, y1),
                source.get_pixel(x1, y1),
            ];
            let alpha_sum: u32 = samples.iter().map(|pixel| u32::from(pixel[3])).sum();
            let alpha = ((alpha_sum + 2) / 4) as u8;
            let mut average = [0u8; 4];
            average[3] = alpha;
            for channel in 0..3 {
                let premultiplied_sum: u32 = samples
                    .iter()
                    .map(|pixel| u32::from(pixel[channel]) * u32::from(pixel[3]))
                    .sum();
                average[channel] = (premultiplied_sum + alpha_sum / 2)
                    .checked_div(alpha_sum)
                    .unwrap_or_default() as u8;
            }
            output.put_pixel(x, y, image::Rgba(average));
        }
    }
    image::DynamicImage::ImageRgba8(output)
}

fn mermaid_command() -> Command {
    #[cfg(windows)]
    {
        let mut command = Command::new("cmd");
        command.args(["/D", "/S", "/C", "mmdc.cmd"]);
        command
    }
    #[cfg(not(windows))]
    {
        Command::new("mmdc")
    }
}

fn path_arg(path: &Path) -> anyhow::Result<&str> {
    path.to_str()
        .ok_or_else(|| anyhow::anyhow!("temporary path is not UTF-8"))
}

struct TempFiles(Vec<PathBuf>);

impl Drop for TempFiles {
    fn drop(&mut self) {
        for path in &self.0 {
            let _ = std::fs::remove_file(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        OUTPUT_DOWNSCALE, Picker, RENDER_SCALE, VIEWPORT_HEIGHT, VIEWPORT_WIDTH,
        downsample_render_scale, key, mermaid_command, mermaid_config, render_diagram,
    };
    use ratatui_image::Resize;

    #[test]
    fn downsample_averages_two_2x2_stages_and_preserves_previous_size() {
        let mut source = image::RgbaImage::new(4, 4);
        for y in 0..4 {
            for x in 0..4 {
                let color = match (x / 2, y / 2) {
                    (0, 0) => 0,
                    (1, 0) => 100,
                    (0, 1) => 200,
                    _ => 255,
                };
                source.put_pixel(x, y, image::Rgba([color, color, color, 255]));
            }
        }
        let result = downsample_render_scale(image::DynamicImage::ImageRgba8(source));
        assert_eq!((result.width(), result.height()), (1, 1));
        assert_eq!(
            result.to_rgba8().get_pixel(0, 0),
            &image::Rgba([139, 139, 139, 255])
        );
    }

    #[test]
    fn cache_key_is_stable_and_content_sensitive() {
        assert_eq!(key("graph TD; A-->B"), key("graph TD; A-->B"));
        assert_ne!(key("graph TD; A-->B"), key("graph TD; A-->C"));
    }

    #[test]
    fn dark_theme_uses_high_contrast_diagram_text_and_edges() {
        let config = mermaid_config();
        assert!(config.contains(r#""theme":"dark""#));
        assert!(config.contains(r#""fontSize":"16px""#));
        assert!(config.contains(r##""lineColor":"#cbd5e1""##));
        assert!(config.contains(r##""primaryTextColor":"#f1f5f9""##));
    }

    #[test]
    fn mmdc_generates_png_and_halfblock_protocol_when_installed() {
        if std::env::var_os("XI_TEST_MERMAID").is_none()
            || mermaid_command().arg("--version").output().is_err()
        {
            return;
        }
        let rendered = render_diagram("graph TD\n  A-->B\n", &Picker::halfblocks())
            .expect("mmdc should render a valid Mermaid graph");
        assert!(rendered.png.starts_with(b"\x89PNG\r\n\x1a\n"));
        let png = image::load_from_memory(&rendered.png).expect("valid PNG");
        assert!(png.width() < u32::from(VIEWPORT_WIDTH) * u32::from(RENDER_SCALE));
        assert!(png.height() < u32::from(VIEWPORT_HEIGHT) * u32::from(RENDER_SCALE));
        let larger = render_diagram(
            "graph LR\n  A[Longer descriptive node label] --> B --> C --> D\n",
            &Picker::halfblocks(),
        )
        .expect("mmdc should render a larger Mermaid graph");
        let larger_png = image::load_from_memory(&larger.png).expect("valid PNG");
        assert_ne!(
            (png.width(), png.height()),
            (larger_png.width(), larger_png.height()),
            "PNG dimensions should follow each diagram's content bounds"
        );
        let physical_image = downsample_render_scale(png.clone());
        let expected_size = Resize::natural_size(&physical_image, Picker::halfblocks().font_size());
        let size = rendered.protocol.size();
        assert_eq!(size, expected_size, "protocol preserves natural image size");
        let physical_size = (physical_image.width(), physical_image.height());
        assert_eq!(
            physical_size,
            (
                png.width()
                    .div_ceil(u32::from(RENDER_SCALE) * u32::from(OUTPUT_DOWNSCALE)),
                png.height()
                    .div_ceil(u32::from(RENDER_SCALE) * u32::from(OUTPUT_DOWNSCALE)),
            ),
            "device-scale pixels plus the requested output reduction must preserve the prior size"
        );
    }
}
