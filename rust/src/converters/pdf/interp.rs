//! The content-stream interpreter: walks page operators through the
//! zero-copy lexer, tracking graphics/text state, and produces raw text
//! items, vector segments, and image placements for page assembly.
//! Marked-content semantics (/ActualText replacement, hidden optional
//! content) are applied here.

use anyhow::Result;
use rustc_hash::{FxHashMap, FxHashSet};

use super::font::{build_font, FontInfo};
use super::geom::{Mat, IDENTITY};
use super::marked_content::{inline_ocg_is_hidden, parse_actual_text};
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

#[derive(Clone)]
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

struct GraphicsState {
    ctm: Mat,
    text: TextState,
    stroke_width: f64,
}

/// Fonts repeat across pages (and form recursions) in virtually every
/// document; building one re-parses widths, encodings, and ToUnicode
/// CMaps from raw bytes. Cache built fonts per font-dict object number
/// for the lifetime of the document.
pub(crate) type FontCache = std::rc::Rc<std::cell::RefCell<FxHashMap<u32, std::rc::Rc<FontInfo>>>>;

pub(crate) struct Interp<'a> {
    pdf: &'a Pdf<'a>,
    font_cache: FontCache,
    /// Raw text items in paint order — page assembly's input.
    pub(crate) items: Vec<RawItem>,
    /// Vector segments (fills/strokes) for table detection.
    pub(crate) segments: Vec<Segment>,
    /// Image geometry and source are coupled so paint-order region
    /// IDs cannot drift away from the stream they identify.
    pub(crate) image_placements: Vec<ImagePlacement<'a>>,
    /// Number of text-showing operators encountered (any font).
    pub(crate) text_ops: usize,
    /// A text-showing operator decoded at least one glyph to a
    /// character — including whitespace. Distinguishes "the encoding is
    /// unsupported" from "the page's only text is blank".
    pub(crate) any_decoded: bool,
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
    state_stack: Vec<GraphicsState>,
    ts: TextState,
    stroke_width: f64,
    // path state for segment building
    path_start: Option<(f64, f64)>,
    path_cur: Option<(f64, f64)>,
    path_segments: Vec<(f64, f64, f64, f64)>, // raw line segments (user space, CTM applied)
    path_rects: Vec<(f64, f64, f64, f64)>,    // re ops: x, y, w, h (CTM applied, axis-aligned)
    depth: usize,
}

impl<'a> Interp<'a> {
    pub(crate) fn clip_to_page(&mut self, width: f64, height: f64) {
        let clip = (0.0, 0.0, width, height);
        clip_items(&mut self.items, 0, clip);
        clip_segments(&mut self.segments, 0, clip);
        clip_images(&mut self.image_placements, 0, clip);
    }

    /// A fresh interpreter for one page. The base CTM is the page's
    /// origin/rotation transform; hidden_ocgs the document's OFF set.
    pub(crate) fn new(
        pdf: &'a Pdf<'a>,
        base: Mat,
        hidden_ocgs: std::rc::Rc<FxHashSet<u32>>,
        font_cache: FontCache,
    ) -> Self {
        Interp {
            pdf,
            font_cache,
            items: Vec::new(),
            segments: Vec::new(),
            image_placements: Vec::new(),
            text_ops: 0,
            any_decoded: false,
            unsupported_font: false,
            mc_depth: 0,
            actual_text: None,
            hidden_ocgs,
            hidden_until: None,
            ctm: base,
            state_stack: Vec::new(),
            ts: TextState::default(),
            stroke_width: 1.0,
            path_start: None,
            path_cur: None,
            path_segments: Vec::new(),
            path_rects: Vec::new(),
            depth: 0,
        }
    }

    pub(crate) fn run(&mut self, content: &[u8], resources: Option<&Dict<'a>>) -> Result<()> {
        use super::content_lex::{Lexer, Operand};

        if self.depth > 6 {
            return Ok(()); // form recursion cap
        }
        let fonts = self.font_map(resources);
        // The XObject dictionary is resolved (and its tree cloned) once
        // per run, lazily — not once per Do operator.
        let mut xobjects: Option<Option<Dict<'a>>> = None;
        let mut lex = Lexer::new(content);

        while let Some(op) = lex.next_op() {
            let n = |i: usize| -> Option<f64> {
                match lex.operands.get(i) {
                    Some(Operand::Num(v)) => Some(*v),
                    _ => None,
                }
            };
            match op {
                b"q" => self.state_stack.push(GraphicsState {
                    ctm: self.ctm,
                    text: self.ts.clone(),
                    stroke_width: self.stroke_width,
                }),
                b"Q" => {
                    if let Some(saved) = self.state_stack.pop() {
                        // Text matrices are not part of the graphics state;
                        // text parameters and the selected font are.
                        let tm = self.ts.tm;
                        let tlm = self.ts.tlm;
                        self.ctm = saved.ctm;
                        self.ts = saved.text;
                        self.ts.tm = tm;
                        self.ts.tlm = tlm;
                        self.stroke_width = saved.stroke_width;
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
                b"w" => {
                    if let Some(width) = n(0).filter(|width| width.is_finite()) {
                        self.stroke_width = width.abs();
                    }
                }
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
                        let (x0, y0, x1, y1) = self.ctm.rect_bbox(x, y, x + w, y + h);
                        self.path_rects.push((x0, y0, x1 - x0, y1 - y0));
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
                        let xd = xobjects.get_or_insert_with(|| {
                            match resources
                                .and_then(|res| self.pdf.dict_get(res, b"XObject").ok().flatten())
                            {
                                Some(Val::Dict(d)) => Some(d),
                                _ => None,
                            }
                        });
                        if let Some(xd) = xd {
                            self.do_xobject(lex.name_bytes(o), xd, resources);
                        }
                    }
                }
                b"BMC" => self.mc_depth += 1,
                b"BDC" => {
                    self.mc_depth += 1;
                    // /OC spans referencing an OCG that is OFF in the
                    // default configuration are invisible: suppress
                    // their content like a viewer would.
                    if self.hidden_until.is_none() && !self.hidden_ocgs.is_empty() {
                        match lex.operands.as_slice() {
                            [tag @ Operand::Name { .. }, property @ Operand::Name { .. }]
                                if lex.name_bytes(*tag) == b"OC"
                                    && self.ocg_hidden(lex.name_bytes(*property), resources) =>
                            {
                                self.hidden_until = Some(self.mc_depth);
                            }
                            [tag @ Operand::Name { .. }, property @ Operand::Dict { .. }]
                                if lex.name_bytes(*tag) == b"OC"
                                    && inline_ocg_is_hidden(
                                        lex.dict_bytes(*property),
                                        &self.hidden_ocgs,
                                    ) =>
                            {
                                self.hidden_until = Some(self.mc_depth);
                            }
                            _ => {}
                        }
                    }
                    // /ActualText in the property dict replaces whatever
                    // the enclosed operators draw, per the PDF graphics model.
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
                    if self.hidden_until.is_some() {
                        lex.clear();
                        continue;
                    }
                    self.image_placements.push(ImagePlacement {
                        bbox: self.ctm.rect_bbox(0.0, 0.0, 1.0, 1.0),
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

    fn optional_content_hidden(&self, value: &Val<'a>) -> bool {
        match value {
            Val::Ref(number) if self.hidden_ocgs.contains(number) => true,
            Val::Ref(number) => match self.pdf.object(*number) {
                Ok(Val::Dict(dict)) => self.optional_content_dict_hidden(&dict),
                _ => false,
            },
            Val::Dict(dict) => self.optional_content_dict_hidden(dict),
            _ => false,
        }
    }

    fn optional_content_dict_hidden(&self, dict: &Dict<'a>) -> bool {
        match dget(dict, b"OCGs") {
            Some(Val::Ref(number)) => self.hidden_ocgs.contains(number),
            Some(Val::Array(groups)) => groups.iter().any(
                |group| matches!(group, Val::Ref(number) if self.hidden_ocgs.contains(number)),
            ),
            _ => false,
        }
    }

    /// Emit the replacement text of a closed /ActualText span using the
    /// geometry its suppressed runs accumulated.
    fn emit_actual_text(&mut self, span: ActualTextSpan) {
        if span.text.trim().is_empty() {
            return;
        }
        let Some((x0, y0, x1, y1, size_dev, bold)) = span.geom else {
            return;
        };
        self.items.push(RawItem {
            text: span.text,
            x: x0,
            y: y0,
            width: (x1 - x0).abs().max(0.01),
            height: (y1 - y0).max(0.01),
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
                    // Referenced fonts hit the document-level cache;
                    // inline font dicts (rare) build uncached.
                    if let Val::Ref(num) = obj {
                        if let Some(cached) = self.font_cache.borrow().get(num) {
                            map.insert(name.to_vec(), cached.clone());
                            continue;
                        }
                    }
                    if let Ok(Val::Dict(fd)) = self.pdf.resolve(obj) {
                        let built = std::rc::Rc::new(build_font(self.pdf, &fd));
                        if let Val::Ref(num) = obj {
                            self.font_cache.borrow_mut().insert(*num, built.clone());
                        }
                        map.insert(name.to_vec(), built);
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

        let size_dev = self.ts.size * self.ts.tm.mul(self.ctm).y_scale();
        // One byte of input ≈ one byte of UTF-8 output for simple fonts;
        // pre-sizing avoids the realloc ladder on every text operator.
        let mut text = String::with_capacity(bytes.len());
        let mut advance = 0.0f64; // text-space units (pre-scale)

        // Typewriter-style tables draw a whole row as ONE string with
        // runs of spaces as column separators; the intra-string
        // geometry would be lost in a single item. Track runs of ≥2
        // space glyphs as split marks:
        // (part_end_len, part_end_adv, next_start_len, next_start_adv).
        let mut split_marks: Vec<(usize, f64, usize, f64)> = Vec::new();
        let mut space_run = 0usize;
        let mut run_start_len = 0usize;
        let mut run_start_adv = 0.0f64;

        // Per-code work, streamed — no per-operator Vec of codes. Only
        // the CJK path materializes (variable-length decode).
        let mut per_code = |code: u32, is_space_byte: bool, uni_override: Option<char>| {
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
            let pre_adv = advance;
            let pre_len = text.len();
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

            // Space-run tracking for the column splitter: a run of ≥2
            // spaces, or a single space stretched past 0.75em by
            // word-spacing (the tabular-Tw trick), separates columns.
            if text.len() != pre_len {
                let piece = &text.as_bytes()[pre_len..];
                if piece == b" " {
                    if advance - pre_adv > 0.75 * self.ts.size {
                        if pre_len > 0 {
                            split_marks.push((pre_len, pre_adv, text.len(), advance));
                        }
                        space_run = 0;
                    } else {
                        if space_run == 0 {
                            run_start_len = pre_len;
                            run_start_adv = pre_adv;
                        }
                        space_run += 1;
                    }
                } else {
                    if space_run >= 2 && run_start_len > 0 {
                        split_marks.push((run_start_len, run_start_adv, pre_len, pre_adv));
                    }
                    space_run = 0;
                }
            }
        };

        if let Some(cjk) = &font.cjk {
            // Variable-length codes; unicode comes straight from the
            // ordering's CID->Unicode table.
            for (cid, uni) in cjk.decode(bytes) {
                per_code(cid, false, uni);
            }
        } else if font.two_byte {
            for c in bytes.chunks(2) {
                let v = if c.len() == 2 {
                    ((c[0] as u32) << 8) | c[1] as u32
                } else {
                    c[0] as u32
                };
                per_code(v, false, None);
            }
        } else {
            for &b in bytes {
                per_code(b as u32, b == 32, None);
            }
        }

        if !text.is_empty() {
            self.any_decoded = true;
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

        let advance_em = advance / self.ts.size.max(1e-9);
        let (box_x0, box_y0, box_x1, box_y1) = if font.vertical {
            trm.rect_bbox(0.0, -advance_em, 1.0, 1.0)
        } else {
            trm.rect_bbox(0.0, -0.20, advance_em, 1.0)
        };

        if let Some(span) = &mut self.actual_text {
            // Geometry only: the replacement text supersedes the glyphs.
            let bold = self.ts.font.as_ref().is_some_and(|f| f.is_bold);
            match &mut span.geom {
                Some((gx0, gy0, gx1, gy1, gsize, gbold)) => {
                    *gx0 = gx0.min(box_x0);
                    *gy0 = gy0.min(box_y0);
                    *gx1 = gx1.max(box_x1);
                    *gy1 = gy1.max(box_y1);
                    *gsize = gsize.max(size_dev);
                    *gbold |= bold;
                }
                geom @ None => {
                    *geom = Some((box_x0, box_y0, box_x1, box_y1, size_dev, bold));
                }
            }
            return;
        }

        if text.trim().is_empty() {
            return;
        }

        if split_marks.is_empty() || font.vertical {
            self.items.push(RawItem {
                text,
                x: box_x0,
                y: box_y0,
                width: (box_x1 - box_x0).max(0.01),
                height: (box_y1 - box_y0).max(0.01),
                // Preserve the established integer-size normalization; heading detection
                // clusters by size, so match that quantization.
                font_size: (size_dev as i32) as f64,
                is_bold: font.is_bold,
            });
            return;
        }

        // Emit one item per multi-space-separated part, each with its
        // own advance-range geometry.
        let size = self.ts.size.max(1e-9);
        let mut seg_len = 0usize;
        let mut seg_adv = 0.0f64;
        let emit = |items: &mut Vec<RawItem>, t: &str, a0: f64, a1: f64| {
            if t.trim().is_empty() {
                return;
            }
            let (x0, y0, x1, y1) = trm.rect_bbox(a0 / size, -0.20, a1 / size, 1.0);
            items.push(RawItem {
                text: t.to_string(),
                x: x0,
                y: y0,
                width: (x1 - x0).max(0.01),
                height: (y1 - y0).max(0.01),
                font_size: (size_dev as i32) as f64,
                is_bold: font.is_bold,
            });
        };
        for &(end_len, end_adv, next_len, next_adv) in &split_marks {
            emit(&mut self.items, &text[seg_len..end_len], seg_adv, end_adv);
            seg_len = next_len;
            seg_adv = next_adv;
        }
        emit(&mut self.items, &text[seg_len..], seg_adv, advance);
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
        // Segment ids are diagnostic-only (nothing reads them in the
        // pipeline): construct them empty rather than format!-ing one
        // String per path operator.
        let effective_stroke_width = self.stroke_width * self.ctm.x_scale().max(self.ctm.y_scale());
        let paints_thin_stroke = paint.strokes() && effective_stroke_width <= 3.0;
        for (x, y, w, h) in std::mem::take(&mut self.path_rects) {
            let filled_rule = if paint.fills() {
                super::shared::thin_rect_to_segment_pub(x, y, w, h)
            } else {
                None
            };
            if let Some(segment) = filled_rule {
                self.segments.push(segment);
            } else if paints_thin_stroke {
                super::shared::push_stroked_rect_edges_pub(&mut self.segments, x, y, w, h);
            }
        }
        if paints_thin_stroke {
            for (x1, y1, x2, y2) in std::mem::take(&mut self.path_segments) {
                // Only axis-aligned-ish lines matter for table grids.
                if (x1 - x2).abs() < 0.8 || (y1 - y2).abs() < 0.8 {
                    self.segments.push(Segment {
                        id: String::new(),
                        x1,
                        y1,
                        x2,
                        y2,
                    });
                }
            }
        }
        self.clear_path();
    }

    fn do_xobject(&mut self, name: &[u8], xobjects: &Dict<'a>, resources: Option<&Dict<'a>>) {
        let Some(obj) = dget(xobjects, name) else {
            return;
        };
        let Ok(Val::Stream(sdict, raw)) = self.pdf.resolve(obj) else {
            return;
        };
        let subtype = dget(&sdict, b"Subtype")
            .and_then(|v| v.as_name())
            .unwrap_or(b"");
        if dget(&sdict, b"OC").is_some_and(|oc| self.optional_content_hidden(oc)) {
            return;
        }

        if subtype == b"Image" {
            if self.hidden_until.is_some() {
                return;
            }
            // Unit square through the CTM.
            self.image_placements.push(ImagePlacement {
                bbox: self.ctm.rect_bbox(0.0, 0.0, 1.0, 1.0),
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
            // A Form XObject executes with an implicit graphics-state save.
            // Its text and stroke parameters must not leak to the caller.
            let saved_ctm = self.ctm;
            let saved_text = self.ts.clone();
            let saved_stroke_width = self.stroke_width;
            let saved_stack_len = self.state_stack.len();
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
            let clip = match dget(&sdict, b"BBox") {
                Some(Val::Array(values)) => {
                    let values: Vec<f64> = values.iter().filter_map(Val::as_num).collect();
                    (values.len() == 4).then(|| {
                        self.ctm
                            .rect_bbox(values[0], values[1], values[2], values[3])
                    })
                }
                _ => None,
            };
            let item_start = self.items.len();
            let segment_start = self.segments.len();
            let image_start = self.image_placements.len();
            self.depth += 1;
            let _ = match &form_res {
                Some(fr) => self.run(&data, Some(fr)),
                None => self.run(&data, resources),
            };
            self.depth -= 1;
            if let Some(clip) = clip {
                clip_items(&mut self.items, item_start, clip);
                clip_segments(&mut self.segments, segment_start, clip);
                clip_images(&mut self.image_placements, image_start, clip);
            }
            self.ctm = saved_ctm;
            self.ts = saved_text;
            self.stroke_width = saved_stroke_width;
            self.state_stack.truncate(saved_stack_len);
        }
    }
}

type ClipRect = (f64, f64, f64, f64);

fn intersects((x0, y0, x1, y1): ClipRect, (cx0, cy0, cx1, cy1): ClipRect) -> bool {
    x1 > cx0 && x0 < cx1 && y1 > cy0 && y0 < cy1
}

/// In-place retain over `v[start..]`: no split_off/extend allocation.
fn retain_tail<T>(v: &mut Vec<T>, start: usize, mut keep: impl FnMut(&mut T) -> bool) {
    let mut write = start;
    for read in start..v.len() {
        if keep(&mut v[read]) {
            v.swap(read, write);
            write += 1;
        }
    }
    v.truncate(write);
}

fn clip_items(items: &mut Vec<RawItem>, start: usize, clip: ClipRect) {
    retain_tail(items, start, |item| {
        let bounds = (item.x, item.y, item.x + item.width, item.y + item.height);
        if !intersects(bounds, clip) {
            return false;
        }
        let x0 = bounds.0.max(clip.0);
        let y0 = bounds.1.max(clip.1);
        let x1 = bounds.2.min(clip.2);
        let y1 = bounds.3.min(clip.3);
        item.x = x0;
        item.y = y0;
        item.width = x1 - x0;
        item.height = y1 - y0;
        true
    });
}

fn clip_segments(segments: &mut Vec<Segment>, start: usize, clip: ClipRect) {
    retain_tail(segments, start, |segment| {
        let bounds = (
            segment.x1.min(segment.x2),
            segment.y1.min(segment.y2),
            segment.x1.max(segment.x2),
            segment.y1.max(segment.y2),
        );
        if !intersects(bounds, clip)
            && !(bounds.0 == bounds.2
                && bounds.0 >= clip.0
                && bounds.0 <= clip.2
                && bounds.3 >= clip.1
                && bounds.1 <= clip.3)
            && !(bounds.1 == bounds.3
                && bounds.1 >= clip.1
                && bounds.1 <= clip.3
                && bounds.2 >= clip.0
                && bounds.0 <= clip.2)
        {
            return false;
        }
        segment.x1 = segment.x1.clamp(clip.0, clip.2);
        segment.x2 = segment.x2.clamp(clip.0, clip.2);
        segment.y1 = segment.y1.clamp(clip.1, clip.3);
        segment.y2 = segment.y2.clamp(clip.1, clip.3);
        true
    });
}

fn clip_images(images: &mut Vec<ImagePlacement<'_>>, start: usize, clip: ClipRect) {
    retain_tail(images, start, |image| {
        if !intersects(image.bbox, clip) {
            return false;
        }
        image.bbox = (
            image.bbox.0.max(clip.0),
            image.bbox.1.max(clip.1),
            image.bbox.2.min(clip.2),
            image.bbox.3.min(clip.3),
        );
        true
    });
}

/// An open /ActualText marked-content span: replacement text plus the
/// geometry accumulated from the suppressed show-text operators.
struct ActualTextSpan {
    text: String,
    depth: u32,
    /// (x0, y0, x1, y1, max_size, any_bold) union across runs.
    geom: Option<(f64, f64, f64, f64, f64, bool)>,
}
