use anyhow::{Context, Result};
use pdfium_render::prelude::*;
use std::path::Path;

#[derive(Debug, Clone)]
pub struct Glyph {
    pub x0: f64,
    pub y0: f64,
    pub x1: f64,
    pub y1: f64,
    pub ch: char,
}

#[derive(Debug, Clone)]
pub struct HLine {
    pub y: f64,
    pub x0: f64,
    pub x1: f64,
    pub height: f64,
}

#[derive(Debug, Clone)]
pub struct VLine {
    pub x: f64,
    pub y0: f64,
    pub y1: f64,
    pub height: f64,
    pub width: f64,
}

#[derive(Debug, Clone)]
pub struct PageLayout {
    pub width: f64,
    pub height: f64,
    pub glyphs: Vec<Glyph>,
    pub hlines: Vec<HLine>,
    pub vlines: Vec<VLine>,
}

pub fn load_page(pdf_path: &Path, page_index: usize) -> Result<PageLayout> {
    let pdfium = Pdfium::default();
    let doc = pdfium
        .load_pdf_from_file(pdf_path, None)
        .with_context(|| format!("open PDF {}", pdf_path.display()))?;
    let page = doc
        .pages()
        .get(page_index as u16)
        .with_context(|| format!("page index {page_index}"))?;
    let width = page.width().value as f64;
    let height = page.height().value as f64;
    let glyphs = extract_glyphs(&page)?;
    let hlines = extract_hlines(&page);
    let vlines = extract_vlines(&page);
    Ok(PageLayout {
        width,
        height,
        glyphs,
        hlines,
        vlines,
    })
}

fn extract_glyphs(page: &PdfPage) -> Result<Vec<Glyph>> {
    let text = page.text().context("page has no text layer")?;
    let mut out = Vec::new();
    for object in page.objects().iter() {
        let Some(text_obj) = object.as_text_object() else {
            continue;
        };
        let chars = text.chars_for_object(text_obj)?;
        for ch in chars.iter() {
            let Some(unicode) = ch.unicode_string() else {
                continue;
            };
            if unicode == " " {
                continue;
            }
            let c = unicode.chars().next().unwrap_or('?');
            if c.is_whitespace() {
                continue;
            }
            let bounds = ch
                .tight_bounds()
                .or_else(|_| ch.loose_bounds())
                .context("glyph bounds")?;
            out.push(Glyph {
                x0: bounds.left().value as f64,
                y0: bounds.bottom().value as f64,
                x1: bounds.right().value as f64,
                y1: bounds.top().value as f64,
                ch: c,
            });
        }
    }
    Ok(out)
}

fn extract_vlines(page: &PdfPage) -> Vec<VLine> {
    let mut out = Vec::new();
    for object in page.objects().iter() {
        let Some(path) = object.as_path_object() else {
            continue;
        };
        let Ok(bounds) = path.bounds() else {
            continue;
        };
        let w = bounds.width().value as f64;
        let h = bounds.height().value as f64;
        if h < 2.0 || w > 4.0 || w < 0.05 || h < w * 1.5 {
            continue;
        }
        out.push(VLine {
            x: (bounds.left().value + bounds.right().value) as f64 / 2.0,
            y0: bounds.bottom().value as f64,
            y1: bounds.top().value as f64,
            height: h,
            width: w,
        });
    }
    out
}

fn extract_hlines(page: &PdfPage) -> Vec<HLine> {
    let mut out = Vec::new();
    for object in page.objects().iter() {
        let Some(path) = object.as_path_object() else {
            continue;
        };
        let Ok(bounds) = path.bounds() else {
            continue;
        };
        let w = bounds.width().value as f64;
        let h = bounds.height().value as f64;
        if w < 15.0 || h > 4.0 || h < 0.05 {
            continue;
        }
        out.push(HLine {
            y: (bounds.bottom().value + bounds.top().value) as f64 / 2.0,
            x0: bounds.left().value as f64,
            x1: bounds.right().value as f64,
            height: h,
        });
    }
    out
}
