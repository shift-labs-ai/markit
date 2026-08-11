//! The content-stream interpreter: walks page operators through the
//! zero-copy lexer, tracking graphics/text state, and produces raw text
//! items, vector segments, and image placements for page assembly.
//! Marked-content semantics (/ActualText replacement, hidden optional
//! content) are applied here.

use anyhow::Result;
use rustc_hash::{FxHashMap, FxHashSet};

use super::font::{build_font, FontInfo};
use super::geom::{Mat, IDENTITY};
use super::own_pdf::{decode_stream, dget, Dict, Pdf, Val};
use super::types::Segment;

pub(crate) struct RawItem {
    pub text: String,
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    pub font_size: f64,
    pub is_bold: bool,
}

struct TextState {
    font: Option<std::rc::Rc<FontInfo>>,
    size: f64,
    char_spacing: f64,
    word_spacing: f64,
    h_scale: f64,
    leading: f64,
    rise: f64,
    tm: Mat,
    tlm: Mat,
}

impl Default for TextState {
    fn default() -> Self {
        TextState {
            font: None,
            size: 0.0,
            char_spacing: 0.0,
            word_spacing: 0.0,
            h_scale: 1.0,
            leading: 0.0,
            rise: 0.0,
            tm: IDENTITY,
            tlm: IDENTITY,
        }
    }
}

pub(crate) type ImageBbox = (f64, f64, f64, f64);

#[derive(Clone)]
pub(crate) enum ImageSource<'a> {
    Inline,
    XObject { dict: Dict<'a>, raw: &'a [u8] },
}

#[derive(Clone)]
pub(crate) struct ImagePlacement<'a> {
    pub(crate) bbox: ImageBbox,
    pub(crate) source: ImageSource<'a>,
}

#[derive(Clone, Copy)]
enum PathPaint {
    Fill,
    Stroke,
    Both,
}

impl PathPaint {
    fn fills(self) -> bool {
        matches!(self, Self::Fill | Self::Both)
    }

    fn strokes(self) -> bool {
        matches!(self, Self::Stroke | Self::Both)
    }
}

pub(crate) struct Interp<'a> {
    pdf: &'a Pdf<'a>,
    /// Raw text items in paint order — page assembly's input.
    pub(crate) items: Vec<RawItem>,
    /// Vector segments (fills/strokes) for table detection.
    pub(crate) segments: Vec<Segment>,
    /// Image geometry and source are coupled so paint-order region
    /// IDs cannot drift away from the stream they identify.
    pub(crate) image_placements: Vec<ImagePlacement<'a>>,
    /// Number of text-showing operators encountered (any font).
    pub(crate) text_ops: usize,
    /// A font with an unsupported predefined CMap showed text: the page
    /// cannot be decoded faithfully.
    pub(crate) unsupported_font: bool,
    /// Marked-content nesting depth, and the depth at which an
    /// /ActualText span opened (replacement text captured until the
    /// matching EMC).
    mc_depth: u32,
    actual_text: Option<ActualTextSpan>,
    /// Object numbers of OCGs OFF in the default configuration.
    hidden_ocgs: std::rc::Rc<FxHashSet<u32>>,
    /// Depth of the outermost hidden optional-content span, when inside.
    hidden_until: Option<u32>,
    ctm: Mat,
    ctm_stack: Vec<Mat>,
    ts: TextState,
    // path state for segment building
    path_start: Option<(f64, f64)>,
    path_cur: Option<(f64, f64)>,
    path_segments: Vec<(f64, f64, f64, f64)>, // raw line segments (user space, CTM applied)
    path_rects: Vec<(f64, f64, f64, f64)>,    // re ops: x, y, w, h (CTM applied, axis-aligned)
    seg_counter: usize,
    page_number: u32,
    depth: usize,
}

impl<'a> Interp<'a> {
    /// A fresh interpreter for one page. The base CTM is the page's
    /// origin/rotation transform; hidden_ocgs the document's OFF set.
    pub(crate) fn new(
        pdf: &'a Pdf<'a>,
        page_number: u32,
        base: Mat,
        hidden_ocgs: std::rc::Rc<FxHashSet<u32>>,
    ) -> Self {
        Interp {
            pdf,
            items: Vec::new(),
            segments: Vec::new(),
            image_placements: Vec::new(),
            text_ops: 0,
            unsupported_font: false,
            mc_depth: 0,
            actual_text: None,
            hidden_ocgs,
            hidden_until: None,
            ctm: base,
            ctm_stack: Vec::new(),
            ts: TextState::default(),
            path_start: None,
            path_cur: None,
            path_segments: Vec::new(),
            path_rects: Vec::new(),
            seg_counter: 0,
            page_number,
            depth: 0,
        }
    }

    pub(crate) fn run(&mut self, content: &[u8], resources: Option<&Dict<'a>>) -> Result<()> {
        use super::content_lex::{Lexer, Operand};

        if self.depth > 6 {
            return Ok(()); // form recursion cap
        }
        let fonts = self.font_map(resources);
        let mut lex = Lexer::new(content);

        while let Some(op) = lex.next_op() {
            let n = |i: usize| -> Option<f64> {
                match lex.operands.get(i) {
                    Some(Operand::Num(v)) => Some(*v),
                    _ => None,
                }
            };
            match op {
                b"q" => self.ctm_stack.push(self.ctm),
                b"Q" => {
                    if let Some(m) = self.ctm_stack.pop() {
                        self.ctm = m;
                    }
                }
                b"cm" => {
                    if let (Some(a), Some(b), Some(c), Some(d), Some(e), Some(f)) =
                        (n(0), n(1), n(2), n(3), n(4), n(5))
                    {
                        self.ctm = Mat { a, b, c, d, e, f }.mul(self.ctm);
                    }
                }
                b"BT" => {
                    self.ts.tm = IDENTITY;
                    self.ts.tlm = IDENTITY;
                }
                b"ET" => {}
                b"Tf" => {
                    if lex.operands.len() == 2 {
                        let name = lex.name_bytes(lex.operands[0]);
                        self.ts.font = fonts.get(name).cloned();
                        self.ts.size = n(1).unwrap_or(0.0);
                    }
                }
                b"Td" => {
                    if let (Some(tx), Some(ty)) = (n(0), n(1)) {
                        self.td(tx, ty);
                    }
                }
                b"TD" => {
                    if let (Some(tx), Some(ty)) = (n(0), n(1)) {
                        self.ts.leading = -ty;
                        self.td(tx, ty);
                    }
                }
                b"Tm" => {
                    if let (Some(a), Some(b), Some(c), Some(d), Some(e), Some(f)) =
                        (n(0), n(1), n(2), n(3), n(4), n(5))
                    {
                        let m = Mat { a, b, c, d, e, f };
                        self.ts.tm = m;
                        self.ts.tlm = m;
                    }
                }
                b"T*" => self.next_line(),
                b"TL" => self.ts.leading = n(0).unwrap_or(0.0),
                b"Tc" => self.ts.char_spacing = n(0).unwrap_or(0.0),
                b"Tw" => self.ts.word_spacing = n(0).unwrap_or(0.0),
                b"Tz" => self.ts.h_scale = n(0).unwrap_or(100.0) / 100.0,
                b"Ts" => self.ts.rise = n(0).unwrap_or(0.0),
                b"Tj" => {
                    if let Some(o @ Operand::Str { .. }) = lex.operands.first().copied() {
                        self.show_text(lex.str_bytes(o));
                    }
                }
                b"'" => {
                    self.next_line();
                    if let Some(o @ Operand::Str { .. }) = lex.operands.first().copied() {
                        self.show_text(lex.str_bytes(o));
                    }
                }
                b"\"" => {
                    if lex.operands.len() == 3 {
                        self.ts.word_spacing = n(0).unwrap_or(0.0);
                        self.ts.char_spacing = n(1).unwrap_or(0.0);
                        self.next_line();
                        if let o @ Operand::Str { .. } = lex.operands[2] {
                            self.show_text(lex.str_bytes(o));
                        }
                    }
                }
                b"TJ" => {
                    for i in 0..lex.operands.len() {
                        match lex.operands[i] {
                            o @ Operand::Str { .. } => self.show_text(lex.str_bytes(o)),
                            Operand::Num(v) => self.kern(v),
                            _ => {}
                        }
                    }
                }
                // ── paths → segments ────────────────────────────────
                b"m" => {
                    if let (Some(x), Some(y)) = (n(0), n(1)) {
                        let p = self.ctm.apply(x, y);
                        self.path_start = Some(p);
                        self.path_cur = Some(p);
                    }
                }
                b"l" => {
                    if let (Some(x), Some(y)) = (n(0), n(1)) {
                        let p = self.ctm.apply(x, y);
                        if let Some(c) = self.path_cur {
                            self.path_segments.push((c.0, c.1, p.0, p.1));
                        }
                        self.path_cur = Some(p);
                    }
                }
                b"c" | b"v" | b"y" => {
                    let k = lex.operands.len();
                    if k >= 2 {
                        if let (Some(x), Some(y)) = (n(k - 2), n(k - 1)) {
                            self.path_cur = Some(self.ctm.apply(x, y));
                        }
                    }
                }
                b"h" => self.close_path(),
                b"re" => {
                    if let (Some(x), Some(y), Some(w), Some(h)) = (n(0), n(1), n(2), n(3)) {
                        let (p0, p1) = (self.ctm.apply(x, y), self.ctm.apply(x + w, y + h));
                        self.path_rects.push((
                            p0.0.min(p1.0),
                            p0.1.min(p1.1),
                            (p1.0 - p0.0).abs(),
                            (p1.1 - p0.1).abs(),
                        ));
                    }
                }
                b"S" => self.flush_path(PathPaint::Stroke),
                b"B" | b"B*" => self.flush_path(PathPaint::Both),
                b"s" => {
                    self.close_path();
                    self.flush_path(PathPaint::Stroke);
                }
                b"b" | b"b*" => {
                    self.close_path();
                    self.flush_path(PathPaint::Both);
                }
                b"f" | b"F" | b"f*" => self.flush_path(PathPaint::Fill),
                b"n" => self.clear_path(),
                // ── XObjects: forms (recurse) + images (bbox) ───────
                b"Do" => {
                    if let Some(o @ Operand::Name { .. }) = lex.operands.first().copied() {
                        let name = lex.name_bytes(o).to_vec();
                        self.do_xobject(&name, resources);
                    }
                }
                b"BMC" => self.mc_depth += 1,
                b"BDC" => {
                    self.mc_depth += 1;
                    // /OC spans referencing an OCG that is OFF in the
                    // default configuration are invisible: suppress
                    // their content like a viewer would.
                    if self.hidden_until.is_none() && !self.hidden_ocgs.is_empty() {
                        if let [o1 @ Operand::Name { .. }, o2 @ Operand::Name { .. }] =
                            lex.operands.as_slice()
                        {
                            if lex.name_bytes(*o1) == b"OC"
                                && self.ocg_hidden(lex.name_bytes(*o2), resources)
                            {
                                self.hidden_until = Some(self.mc_depth);
                            }
                        }
                    }
                    // /ActualText in the property dict replaces whatever
                    // the enclosed operators draw (MuPDF honors this).
                    if self.actual_text.is_none() {
                        if let Some(o @ Operand::Dict { .. }) = lex.operands.last().copied() {
                            if let Some(s) = parse_actual_text(lex.dict_bytes(o)) {
                                self.actual_text = Some(ActualTextSpan {
                                    text: s,
                                    depth: self.mc_depth,
                                    geom: None,
                                });
                            }
                        }
                    }
                }
                b"EMC" => {
                    if self.hidden_until == Some(self.mc_depth) {
                        self.hidden_until = None;
                    }
                    if let Some(span) = &self.actual_text {
                        if span.depth == self.mc_depth {
                            let span = self.actual_text.take().unwrap();
                            self.emit_actual_text(span);
                        }
                    }
                    self.mc_depth = self.mc_depth.saturating_sub(1);
                }
                // Inline image (BI…ID…EI, payload skipped by the lexer):
                // record the placement so image regions and scanned-page
                // detection see it. The empty payload marker keeps the
                // bbox/xobject pairing aligned; extraction of these
                // regions falls back to rasterization.
                b"BI" => {
                    let (ax, ay) = self.ctm.apply(0.0, 0.0);
                    let (bx, by) = self.ctm.apply(1.0, 1.0);
                    self.image_placements.push(ImagePlacement {
                        bbox: (ax.min(bx), ay.min(by), ax.max(bx), ay.max(by)),
                        source: ImageSource::Inline,
                    });
                }
                _ => {}
            }
            lex.clear();
        }
        Ok(())
    }

    /// Is the named /Properties entry an OCG (or OCMD) that the default
    /// configuration turns OFF?
    fn ocg_hidden(&self, name: &[u8], resources: Option<&Dict<'a>>) -> bool {
        let Some(res) = resources else { return false };
        let Ok(Some(Val::Dict(props))) = self.pdf.dict_get(res, b"Properties") else {
            return false;
        };
        match dget(&props, name) {
            Some(Val::Ref(num)) => {
                if self.hidden_ocgs.contains(num) {
                    return true;
                }
                // OCMD: /OCGs holds the actual group refs.
                if let Ok(Val::Dict(d)) = self.pdf.object(*num) {
                    match dget(&d, b"OCGs") {
                        Some(Val::Ref(n)) => return self.hidden_ocgs.contains(n),
                        Some(Val::Array(a)) => {
                            return a
                                .iter()
                                .any(|v| matches!(v, Val::Ref(n) if self.hidden_ocgs.contains(n)))
                        }
                        _ => {}
                    }
                }
                false
            }
            _ => false,
        }
    }

    /// Emit the replacement text of a closed /ActualText span using the
    /// geometry its suppressed runs accumulated.
    fn emit_actual_text(&mut self, span: ActualTextSpan) {
        if span.text.trim().is_empty() {
            return;
        }
        let Some((x0, y0, x1, size_dev, bold)) = span.geom else {
            return;
        };
        self.items.push(RawItem {
            text: span.text,
            x: x0,
            y: y0 - 0.20 * size_dev,
            width: (x1 - x0).abs().max(0.01),
            height: 1.2 * size_dev,
            font_size: (size_dev as i32) as f64,
            is_bold: bold,
        });
    }

    fn td(&mut self, tx: f64, ty: f64) {
        self.ts.tlm = Mat {
            a: 1.0,
            b: 0.0,
            c: 0.0,
            d: 1.0,
            e: tx,
            f: ty,
        }
        .mul(self.ts.tlm);
        self.ts.tm = self.ts.tlm;
    }

    fn font_map(&self, resources: Option<&Dict<'a>>) -> FxHashMap<Vec<u8>, std::rc::Rc<FontInfo>> {
        let mut map = FxHashMap::default();
        if let Some(res) = resources {
            if let Ok(Some(Val::Dict(fonts))) = self.pdf.dict_get(res, b"Font") {
                for (name, obj) in &fonts {
                    if let Ok(Val::Dict(fd)) = self.pdf.resolve(obj) {
                        map.insert(name.to_vec(), std::rc::Rc::new(build_font(self.pdf, &fd)));
                    }
                }
            }
        }
        map
    }

    fn next_line(&mut self) {
        let l = self.ts.leading;
        self.ts.tlm = Mat {
            a: 1.0,
            b: 0.0,
            c: 0.0,
            d: 1.0,
            e: 0.0,
            f: -l,
        }
        .mul(self.ts.tlm);
        self.ts.tm = self.ts.tlm;
    }

    fn kern(&mut self, amount: f64) {
        // TJ number: translate by -amount/1000 × size × h_scale in text space.
        let tx = -amount / 1000.0 * self.ts.size * self.ts.h_scale;
        self.ts.tm = Mat {
            a: 1.0,
            b: 0.0,
            c: 0.0,
            d: 1.0,
            e: tx,
            f: 0.0,
        }
        .mul(self.ts.tm);
    }

    fn show_text(&mut self, bytes: &[u8]) {
        self.text_ops += 1;
        if self.hidden_until.is_some() {
            // Inside an OFF optional-content span: invisible.
            return;
        }
        if self.ts.font.as_ref().is_some_and(|f| f.unsupported_cmap) {
            self.unsupported_font = true;
            return;
        }
        let Some(font) = self.ts.font.clone() else {
            return;
        };
        if self.ts.size == 0.0 {
            return;
        }
        let trm = Mat {
            a: self.ts.size * self.ts.h_scale,
            b: 0.0,
            c: 0.0,
            d: self.ts.size,
            e: 0.0,
            f: self.ts.rise,
        }
        .mul(self.ts.tm)
        .mul(self.ctm);

        let (x0, y0) = trm.apply(0.0, 0.0);
        let size_dev = self.ts.size * self.ts.tm.mul(self.ctm).y_scale();
        let mut text = String::new();
        let mut advance = 0.0f64; // text-space units (pre-scale)

        // (code-or-cid, is-space-byte, resolved unicode override)
        let codes: Vec<(u32, bool, Option<char>)> = if let Some(cjk) = &font.cjk {
            // Variable-length codes; unicode comes straight from the
            // ordering's CID->Unicode table.
            cjk.decode(bytes)
                .into_iter()
                .map(|(cid, uni)| (cid, false, uni))
                .collect()
        } else if font.two_byte {
            bytes
                .chunks(2)
                .map(|c| {
                    let v = if c.len() == 2 {
                        ((c[0] as u32) << 8) | c[1] as u32
                    } else {
                        c[0] as u32
                    };
                    (v, false, None)
                })
                .collect()
        } else {
            bytes.iter().map(|&b| (b as u32, b == 32, None)).collect()
        };

        for (code, is_space_byte, uni_override) in codes {
            let w = if font.two_byte || font.cjk.is_some() {
                *font.cid_widths.get(&code).unwrap_or(&font.default_width)
            } else {
                let w = font.widths[code as usize & 0xff];
                if w != 0.0 {
                    w
                } else if font.default_width != 0.0 {
                    font.default_width
                } else {
                    500.0
                }
            };
            advance += (w / 1000.0 * self.ts.size
                + self.ts.char_spacing
                + if is_space_byte {
                    self.ts.word_spacing
                } else {
                    0.0
                })
                * self.ts.h_scale;

            if let Some(c) = uni_override {
                text.push(c);
            } else if font.cjk.is_some() {
                // Embedded/predefined CMap without a table hit: try
                // ToUnicode (keyed by CID), then the Adobe ordering.
                if let Some(s) = font.to_unicode.get(&code) {
                    text.push_str(s);
                } else if let Some(c) = font.adobe_ordering.as_ref().and_then(|m| m.lookup(code)) {
                    text.push(c);
                }
            } else if font.two_byte {
                if font.ucs2_codes {
                    // Codes are UCS-2 code units directly.
                    if let Some(c) = char::from_u32(code) {
                        text.push(c);
                    }
                } else if let Some(s) = font.to_unicode.get(&code) {
                    text.push_str(s);
                } else if let Some(c) = font.adobe_ordering.as_ref().and_then(|m| m.lookup(code)) {
                    text.push(c);
                } else if let Some(c) = char::from_u32(code) {
                    text.push(c);
                }
            } else if let Some(s) = font.to_unicode.get(&code) {
                text.push_str(s);
            } else if let Some(c) = font.to_unicode_simple[code as usize & 0xff] {
                text.push(c);
            }
        }

        // Advance Tm: horizontal writing moves across, vertical (WMode
        // 1) moves down the page.
        let (tx, ty) = if font.vertical {
            (0.0, -advance)
        } else {
            (advance, 0.0)
        };
        self.ts.tm = Mat {
            a: 1.0,
            b: 0.0,
            c: 0.0,
            d: 1.0,
            e: tx,
            f: ty,
        }
        .mul(self.ts.tm);

        let (x1, _) = trm.apply(advance / self.ts.size.max(1e-9), 0.0);

        if let Some(span) = &mut self.actual_text {
            // Geometry only: the replacement text supersedes the glyphs.
            let bold = self.ts.font.as_ref().is_some_and(|f| f.is_bold);
            match &mut span.geom {
                Some((gx0, _gy0, gx1, gsize, _)) => {
                    *gx0 = gx0.min(x0.min(x1));
                    *gx1 = gx1.max(x0.max(x1));
                    *gsize = gsize.max(size_dev);
                }
                geom @ None => *geom = Some((x0.min(x1), y0, x0.max(x1), size_dev, bold)),
            }
            return;
        }

        if text.trim().is_empty() {
            return;
        }

        let width_dev = (x1 - x0).abs().max(0.01);
        self.items.push(RawItem {
            text,
            x: x0.min(x1),
            y: y0 - 0.20 * size_dev, // approximate descent below baseline
            width: width_dev,
            height: 1.2 * size_dev,
            // MuPDF's stext JSON int-truncates sizes; heading detection
            // clusters by size, so match that quantization.
            font_size: (size_dev as i32) as f64,
            is_bold: font.is_bold,
        });
    }

    fn clear_path(&mut self) {
        self.path_segments.clear();
        self.path_rects.clear();
        self.path_start = None;
        self.path_cur = None;
    }

    fn close_path(&mut self) {
        if let (Some(current), Some(start)) = (self.path_cur, self.path_start) {
            if current != start {
                self.path_segments
                    .push((current.0, current.1, start.0, start.1));
            }
            self.path_cur = Some(start);
        }
    }

    fn flush_path(&mut self, paint: PathPaint) {
        for (x, y, w, h) in std::mem::take(&mut self.path_rects) {
            let filled_rule = if paint.fills() {
                let id = format!("p{}-fr{}", self.page_number, self.seg_counter);
                super::extract::thin_rect_to_segment_pub(id, x, y, w, h)
            } else {
                None
            };
            if let Some(segment) = filled_rule {
                self.seg_counter += 1;
                self.segments.push(segment);
            } else if paint.strokes() {
                let id = format!("p{}-r{}", self.page_number, self.seg_counter);
                self.seg_counter += 1;
                super::extract::push_stroked_rect_edges_pub(&mut self.segments, &id, x, y, w, h);
            }
        }
        if paint.strokes() {
            for (x1, y1, x2, y2) in std::mem::take(&mut self.path_segments) {
                // Only axis-aligned-ish lines matter for table grids.
                if (x1 - x2).abs() < 0.8 || (y1 - y2).abs() < 0.8 {
                    let id = format!("p{}-l{}", self.page_number, self.seg_counter);
                    self.seg_counter += 1;
                    self.segments.push(Segment { id, x1, y1, x2, y2 });
                }
            }
        }
        self.clear_path();
    }

    fn do_xobject(&mut self, name: &[u8], resources: Option<&Dict<'a>>) {
        let Some(res) = resources else { return };
        let Ok(Some(Val::Dict(xobjects))) = self.pdf.dict_get(res, b"XObject") else {
            return;
        };
        let Some(obj) = dget(&xobjects, name) else {
            return;
        };
        let Ok(Val::Stream(sdict, raw)) = self.pdf.resolve(obj) else {
            return;
        };
        let subtype = dget(&sdict, b"Subtype")
            .and_then(|v| v.as_name())
            .unwrap_or(b"");

        if subtype == b"Image" {
            if self.hidden_until.is_some() {
                return;
            }
            // Unit square through the CTM.
            let (ax, ay) = self.ctm.apply(0.0, 0.0);
            let (bx, by) = self.ctm.apply(1.0, 1.0);
            self.image_placements.push(ImagePlacement {
                bbox: (ax.min(bx), ay.min(by), ax.max(bx), ay.max(by)),
                source: ImageSource::XObject {
                    dict: sdict.clone(),
                    raw,
                },
            });
            return;
        }

        if subtype == b"Form" {
            let Ok(data) = decode_stream(&sdict, raw, self.pdf) else {
                return;
            };
            let form_res = match self.pdf.dict_get(&sdict, b"Resources") {
                Ok(Some(Val::Dict(d))) => Some(d),
                _ => None,
            };
            let saved_ctm = self.ctm;
            if let Some(Val::Array(m)) = dget(&sdict, b"Matrix") {
                let v: Vec<f64> = m.iter().filter_map(|o| o.as_num()).collect();
                if v.len() == 6 {
                    self.ctm = Mat {
                        a: v[0],
                        b: v[1],
                        c: v[2],
                        d: v[3],
                        e: v[4],
                        f: v[5],
                    }
                    .mul(self.ctm);
                }
            }
            self.depth += 1;
            let _ = match &form_res {
                Some(fr) => self.run(&data, Some(fr)),
                None => self.run(&data, resources),
            };
            self.depth -= 1;
            self.ctm = saved_ctm;
        }
    }
}

/// Pull /ActualText out of a raw BDC property dict. Handles literal
/// strings (with escapes) and hex strings; UTF-16BE by BOM, else
/// PDFDocEncoding treated as latin1.
fn parse_actual_text(dict: &[u8]) -> Option<String> {
    let at = memchr::memmem::find(dict, b"/ActualText")?;
    let mut p = at + b"/ActualText".len();
    while p < dict.len() && dict[p].is_ascii_whitespace() {
        p += 1;
    }
    let bytes: Vec<u8> = match dict.get(p)? {
        b'(' => {
            let mut out = Vec::new();
            let mut depth = 1usize;
            p += 1;
            while p < dict.len() && depth > 0 {
                match dict[p] {
                    b'\\' => {
                        p += 1;
                        match dict.get(p)? {
                            b'n' => out.push(b'\n'),
                            b'r' => out.push(b'\r'),
                            b't' => out.push(b'\t'),
                            b'b' => out.push(8),
                            b'f' => out.push(12),
                            d @ b'0'..=b'7' => {
                                let mut v = (d - b'0') as u32;
                                for _ in 0..2 {
                                    match dict.get(p + 1) {
                                        Some(d2 @ b'0'..=b'7') => {
                                            v = v * 8 + (d2 - b'0') as u32;
                                            p += 1;
                                        }
                                        _ => break,
                                    }
                                }
                                out.push(v as u8);
                            }
                            &c => out.push(c),
                        }
                        p += 1;
                    }
                    b'(' => {
                        depth += 1;
                        out.push(b'(');
                        p += 1;
                    }
                    b')' => {
                        depth -= 1;
                        if depth > 0 {
                            out.push(b')');
                        }
                        p += 1;
                    }
                    c => {
                        out.push(c);
                        p += 1;
                    }
                }
            }
            out
        }
        b'<' => {
            let end = dict[p..].iter().position(|&b| b == b'>')? + p;
            let mut out = Vec::new();
            let mut hi: Option<u8> = None;
            for &b in &dict[p + 1..end] {
                let v = match b {
                    b'0'..=b'9' => b - b'0',
                    b'a'..=b'f' => b - b'a' + 10,
                    b'A'..=b'F' => b - b'A' + 10,
                    _ => continue,
                };
                match hi.take() {
                    Some(h) => out.push((h << 4) | v),
                    None => hi = Some(v),
                }
            }
            out
        }
        _ => return None,
    };

    if bytes.starts_with(&[0xFE, 0xFF]) {
        let units: Vec<u16> = bytes[2..]
            .chunks_exact(2)
            .map(|c| u16::from_be_bytes([c[0], c[1]]))
            .collect();
        Some(String::from_utf16_lossy(&units))
    } else {
        Some(bytes.iter().map(|&b| b as char).collect())
    }
}

/// An open /ActualText marked-content span: replacement text plus the
/// geometry accumulated from the suppressed show-text operators.
struct ActualTextSpan {
    text: String,
    depth: u32,
    /// (x0, y0, x1, size_dev, is_bold) built up across runs.
    geom: Option<(f64, f64, f64, f64, bool)>,
}
