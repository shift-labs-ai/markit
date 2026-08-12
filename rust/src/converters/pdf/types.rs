//! Shared types for the PDF pipeline.

/// Bounding box in PDF coordinate space (origin = bottom-left).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Bounds {
    pub left: f64,
    pub right: f64,
    /// Higher value = higher on the page.
    pub top: f64,
    pub bottom: f64,
}

/// A text fragment with position and font metadata.
#[derive(Debug, Clone, PartialEq)]
pub struct TextBox {
    pub id: String,
    pub text: String,
    pub bounds: Bounds,
    pub page_number: u32,
    /// Dominant font size in points.
    pub font_size: f64,
    /// True if rendered bold (font name or rendering mode).
    pub is_bold: bool,
}

/// A horizontal or vertical line segment extracted from vector graphics.
#[derive(Debug, Clone, PartialEq)]
pub struct Segment {
    pub id: String,
    pub x1: f64,
    pub y1: f64,
    pub x2: f64,
    pub y2: f64,
}

/// A single cell in a resolved table grid.
#[derive(Debug, Clone, PartialEq)]
pub struct TableCell {
    pub row: usize,
    pub col: usize,
    pub text: String,
    pub row_span: usize,
    pub col_span: usize,
}

/// A resolved table grid ready for markdown rendering.
#[derive(Debug, Clone, PartialEq)]
pub struct TableGrid {
    pub page_number: u32,
    pub rows: usize,
    pub cols: usize,
    pub cells: Vec<TableCell>,
    pub warnings: Vec<String>,
    /// Top Y coordinate (PDF space: larger = higher on page).
    pub top_y: f64,
    /// True for tables detected without vector borders.
    pub is_borderless: bool,
}

/// An image/diagram region detected on a page.
#[derive(Debug, Clone, PartialEq)]
pub struct ImageRegion {
    pub id: String,
    pub page_number: u32,
    /// Bounding box in page coordinates (top-left origin).
    pub bbox: Rect,
    /// Y position in PDF coordinates (bottom-left) for ordering.
    pub top_y: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

/// Result of extracting content from a single PDF page.
#[derive(Debug, Clone, PartialEq)]
pub struct PageContent {
    pub page_number: u32,
    pub page_width: f64,
    pub page_height: f64,
    pub text_boxes: Vec<TextBox>,
    pub segments: Vec<Segment>,
    pub images: Vec<ImageRegion>,
}

/// A block of rendered content (text paragraph or table).
#[derive(Debug, Clone, PartialEq)]
pub struct ContentBlock {
    pub top_y: f64,
    pub content: String,
    /// True if this line has wide gaps between text boxes (column headers).
    pub is_tabular: bool,
}
