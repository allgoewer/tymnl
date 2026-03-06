use chrono::{Datelike, Utc};
use chrono_tz::Tz;
use image::imageops::{self, ColorMap};
use image::{DynamicImage, Luma, RgbaImage};
use png::{BitDepth, ColorType, Encoder};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use tiny_skia::Pixmap;
use typst::diag::{FileError, FileResult, Severity, SourceDiagnostic};
use typst::ecow::EcoVec;
use typst::foundations::{Bytes, Datetime, Dict, Value};
use typst::layout::PagedDocument;
use typst::syntax::{FileId, Source, VirtualPath};
use typst::text::{Font, FontBook};
use typst::utils::LazyHash;
use typst::{Library, LibraryExt as _, World as TypstWorld};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Input/Output error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Encoding to PNG failed: {0}")]
    Encoding(#[from] png::EncodingError),
    #[error("{0}")]
    Typst(String),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Depth {
    Native,
    #[default]
    Bit1,
    Bit2,
}

#[derive(Clone, Debug)]
pub struct Renderer {
    book: LazyHash<FontBook>,
    fonts: Vec<Font>,
    config_dir: PathBuf,
    tymnl_template: Source,
}

impl Renderer {
    pub fn new(config_dir: impl AsRef<Path>) -> Result<Self, Error> {
        let config_dir = config_dir.as_ref();
        let mut fonts = Vec::new();

        // load all default fonts
        for f in typst_assets::fonts() {
            let buffer = Bytes::new(f);

            // load all builtin font faces
            let mut face_index = 0;
            while let Some(font) = Font::new(buffer.clone(), face_index) {
                fonts.push(font);
                face_index += 1;
            }
        }

        // load bundled SpaceGrotesk fonts
        for data in [
            include_bytes!("fonts/SpaceGrotesk-Light.ttf").as_slice(),
            include_bytes!("fonts/SpaceGrotesk-Regular.ttf").as_slice(),
            include_bytes!("fonts/SpaceGrotesk-Medium.ttf").as_slice(),
            include_bytes!("fonts/SpaceGrotesk-SemiBold.ttf").as_slice(),
            include_bytes!("fonts/SpaceGrotesk-Bold.ttf").as_slice(),
        ] {
            let buffer = Bytes::new(data);
            let mut face_index = 0;
            while let Some(font) = Font::new(buffer.clone(), face_index) {
                fonts.push(font);
                face_index += 1;
            }
        }

        // load user fonts
        let mut font_db = fontdb::Database::new();
        font_db.load_fonts_dir(config_dir.join("fonts"));

        let local_fonts: Vec<_> = font_db
            .faces()
            .filter_map(|face| {
                let (src, index) = match &face.source {
                    fontdb::Source::File(path) => (fs::read(path).ok()?, face.index),
                    fontdb::Source::Binary(data) => (data.as_ref().as_ref().to_vec(), face.index),
                    _ => return None,
                };
                Font::new(Bytes::new(src), index)
            })
            .collect();

        fonts.extend(local_fonts);

        let book = FontBook::from_fonts(fonts.iter());

        Ok(Self {
            book: LazyHash::new(book),
            fonts,
            config_dir: config_dir.to_path_buf(),
            tymnl_template: Source::new(
                FileId::new_fake(VirtualPath::new("/tymnl.typ")),
                include_str!("templates/tymnl.typ").to_owned(),
            ),
        })
    }

    pub fn fonts(&self) -> &[Font] {
        &self.fonts
    }

    pub fn render(
        &self,
        source: String,
        inputs: Option<HashMap<String, String>>,
        ppi: f32,
        depth: Depth,
        timezone: Tz,
    ) -> Result<Vec<u8>, Error> {
        let main = Source::new(FileId::new_fake(VirtualPath::new("/main.typ")), source);

        let world = World::new(
            main,
            self.config_dir.clone(),
            self.book.clone(),
            self.fonts.clone(),
            self.tymnl_template.clone(),
            inputs,
            timezone,
        );

        world.render(ppi, depth)
    }
}

#[derive(Clone, Debug)]
struct World {
    library: LazyHash<Library>,
    book: LazyHash<FontBook>,
    fonts: Vec<Font>,
    main: Source,
    config_dir: PathBuf,
    tymnl_template: Source,
    timezone: Tz,
}

impl World {
    fn new(
        main: Source,
        config_dir: PathBuf,
        book: LazyHash<FontBook>,
        fonts: Vec<Font>,
        tymnl_template: Source,
        inputs: Option<HashMap<String, String>>,
        timezone: Tz,
    ) -> Self {
        let library = if let Some(inputs) = inputs {
            let mut dict = Dict::new();
            for (k, v) in inputs {
                dict.insert(k.into(), Value::Str(v.into()));
            }

            let library_builder = Library::builder();
            library_builder.with_inputs(dict).build()
        } else {
            Library::default()
        };

        Self {
            library: LazyHash::new(library),
            book: book.clone(),
            fonts,
            main,
            config_dir,
            tymnl_template,
            timezone,
        }
    }

    fn format_diagnostics(&self, diags: &EcoVec<SourceDiagnostic>) -> String {
        let mut output = String::new();
        for diag in diags {
            let severity = match diag.severity {
                Severity::Error => "error",
                Severity::Warning => "warning",
            };
            output.push_str(&format!("{}: {}\n", severity, diag.message));

            'loc: {
                if diag.span.is_detached() {
                    break 'loc;
                }
                let Some(id) = diag.span.id() else { break 'loc };
                let Ok(source) = self.source(id) else {
                    break 'loc;
                };
                let Some(range) = source.range(diag.span) else {
                    break 'loc;
                };
                let Some((line, col)) = source.lines().byte_to_line_column(range.start) else {
                    break 'loc;
                };

                let path = id.vpath().as_rootless_path().display().to_string();
                output.push_str(&format!("  --> {}:{}:{}\n", path, line + 1, col + 1));

                let Some(line_range) = source.lines().line_to_range(line) else {
                    break 'loc;
                };
                let line_text =
                    source.text()[line_range.start..line_range.end].trim_end_matches(['\n', '\r']);
                let num = (line + 1).to_string();
                let pad = num.len();
                output.push_str(&format!("{} |\n", " ".repeat(pad)));
                output.push_str(&format!("{} | {}\n", num, line_text));
                let caret_count = range
                    .end
                    .min(line_range.end)
                    .saturating_sub(range.start)
                    .max(1);
                output.push_str(&format!(
                    "{} | {}{}\n",
                    " ".repeat(pad),
                    " ".repeat(col),
                    "^".repeat(caret_count),
                ));
            }

            for hint in &diag.hints {
                output.push_str(&format!("  hint: {}\n", hint));
            }
            output.push('\n');
        }
        output.trim_end().to_owned()
    }

    fn render(&self, ppi: f32, depth: Depth) -> Result<Vec<u8>, Error> {
        let doc: PagedDocument = typst::compile(&self)
            .output
            .map_err(|diags| Error::Typst(self.format_diagnostics(&diags)))?;
        let canvas = typst_render::render(&doc.pages[0], ppi / 72.0);

        Ok(match depth {
            Depth::Native => canvas.encode_png()?,
            Depth::Bit2 => process_2bit(&canvas)?,
            Depth::Bit1 => process_1bit(&canvas)?,
        })
    }
}

impl TypstWorld for World {
    fn library(&self) -> &LazyHash<Library> {
        &self.library
    }

    fn book(&self) -> &LazyHash<FontBook> {
        &self.book
    }

    fn main(&self) -> FileId {
        self.main.id()
    }

    fn source(&self, id: FileId) -> FileResult<Source> {
        if self.main.id() == id {
            return Ok(self.main.clone());
        }

        if id.vpath() == self.tymnl_template.id().vpath() {
            return Ok(self.tymnl_template.clone());
        }

        let path = self.config_dir.join(id.vpath().as_rootless_path());
        let text = std::fs::read_to_string(&path).map_err(|_| FileError::NotFound(path))?;

        Ok(Source::new(id, text))
    }

    fn file(&self, id: FileId) -> FileResult<Bytes> {
        let path = self.config_dir.join(id.vpath().as_rootless_path());
        let data = std::fs::read(&path).map_err(|_| FileError::NotFound(path))?;

        Ok(Bytes::new(data))
    }

    fn font(&self, index: usize) -> Option<Font> {
        self.fonts.get(index).cloned()
    }

    fn today(&self, offset: Option<i64>) -> Option<Datetime> {
        let date = if let Some(offset) = offset {
            let tz = chrono::FixedOffset::east_opt((offset * 3600) as i32)?;
            Utc::now().with_timezone(&tz).date_naive()
        } else {
            Utc::now().with_timezone(&self.timezone).date_naive()
        };
        Datetime::from_ymd(date.year() as _, date.month() as _, date.day() as _)
    }
}

fn pixmap_to_dynamic(pixmap: &Pixmap) -> DynamicImage {
    // tiny-skia data is RGBA8888. If you have transparency, you may need
    // to un-premultiply here. For opaque images, direct copy is fine.
    let img = RgbaImage::from_raw(pixmap.width(), pixmap.height(), pixmap.data().to_vec())
        .expect("Buffer size matches");

    DynamicImage::ImageRgba8(img)
}

struct Palette2Bit;

impl ColorMap for Palette2Bit {
    type Color = Luma<u8>;

    fn index_of(&self, color: &Self::Color) -> usize {
        let luma = color[0];
        // Midpoints between 0, 85, 170, and 255
        if luma < 43 {
            0
        } else if luma < 128 {
            1
        } else if luma < 213 {
            2
        } else {
            3
        }
    }

    fn map_color(&self, color: &mut Self::Color) {
        *color = Luma([match self.index_of(color) {
            0 => 0x00, // #000000
            1 => 0x55, // #555555
            2 => 0xAA, // #aaaaaa
            _ => 0xFF, // #ffffff
        }]);
    }
}

fn process_1bit(pixmap: &Pixmap) -> Result<Vec<u8>, Error> {
    let mut img = pixmap_to_dynamic(pixmap).into_luma8();

    for pixel in img.pixels_mut() {
        let val = pixel[0];
        pixel[0] = if val > 160 { 255 } else { 0 };
    }

    let packed = pack_1bit(img.as_raw());

    save_png(&packed, img.width(), img.height(), BitDepth::One)
}

fn pack_1bit(data: &[u8]) -> Vec<u8> {
    data.chunks(8)
        .map(|chunk| {
            let mut byte = 0u8;
            for (i, &p) in chunk.iter().enumerate() {
                if p > 127 {
                    byte |= 1 << (7 - i)
                }
            }
            byte
        })
        .collect()
}

fn process_2bit(pixmap: &Pixmap) -> Result<Vec<u8>, Error> {
    let mut img = pixmap_to_dynamic(pixmap).into_luma8();

    imageops::dither(&mut img, &Palette2Bit);

    let packed = pack_2bit(img.as_raw());

    save_png(&packed, img.width(), img.height(), BitDepth::Two)
}

fn pack_2bit(data: &[u8]) -> Vec<u8> {
    data.chunks(4)
        .map(|chunk| {
            let mut byte = 0u8;
            for (i, &p) in chunk.iter().enumerate() {
                let val = match p {
                    0..=42 => 0b00,
                    43..=127 => 0b01,
                    128..=212 => 0b10,
                    _ => 0b11,
                };
                byte |= val << (6 - (i * 2));
            }
            byte
        })
        .collect()
}

fn save_png(data: &[u8], width: u32, height: u32, depth: BitDepth) -> Result<Vec<u8>, Error> {
    let mut buf = Vec::with_capacity(100 * 1024);

    {
        let mut encoder = Encoder::new(&mut buf, width, height);
        encoder.set_color(ColorType::Grayscale);
        encoder.set_depth(depth);

        let mut writer = encoder.write_header()?;
        writer.write_image_data(data)?;
    }

    Ok(buf)
}
