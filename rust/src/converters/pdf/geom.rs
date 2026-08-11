//! 2D affine geometry for the content interpreter: the PDF matrix
//! [a b c d e f] and the page-level base transform (MediaBox origin
//! normalization composed with /Rotate).

#[derive(Clone, Copy, Debug)]
pub(crate) struct Mat {
    pub(crate) a: f64,
    pub(crate) b: f64,
    pub(crate) c: f64,
    pub(crate) d: f64,
    pub(crate) e: f64,
    pub(crate) f: f64,
}

pub(crate) const IDENTITY: Mat = Mat {
    a: 1.0,
    b: 0.0,
    c: 0.0,
    d: 1.0,
    e: 0.0,
    f: 0.0,
};

impl Mat {
    pub(crate) fn mul(self, m: Mat) -> Mat {
        // self × m (apply self first, then m)
        Mat {
            a: self.a * m.a + self.b * m.c,
            b: self.a * m.b + self.b * m.d,
            c: self.c * m.a + self.d * m.c,
            d: self.c * m.b + self.d * m.d,
            e: self.e * m.a + self.f * m.c + m.e,
            f: self.e * m.b + self.f * m.d + m.f,
        }
    }

    pub(crate) fn apply(&self, x: f64, y: f64) -> (f64, f64) {
        (
            self.a * x + self.c * y + self.e,
            self.b * x + self.d * y + self.f,
        )
    }

    /// Magnitude of the x-axis scale.
    pub(crate) fn x_scale(&self) -> f64 {
        (self.a * self.a + self.b * self.b).sqrt()
    }

    /// Magnitude of the y-axis scale (for font size in device space).
    pub(crate) fn y_scale(&self) -> f64 {
        (self.c * self.c + self.d * self.d).sqrt()
    }

    /// Axis-aligned bounds of a transformed rectangle. All four corners
    /// matter under rotation and shear; a diagonal pair is insufficient.
    pub(crate) fn rect_bbox(&self, x0: f64, y0: f64, x1: f64, y1: f64) -> (f64, f64, f64, f64) {
        let points = [
            self.apply(x0, y0),
            self.apply(x1, y0),
            self.apply(x0, y1),
            self.apply(x1, y1),
        ];
        points.iter().fold(
            (
                f64::INFINITY,
                f64::INFINITY,
                f64::NEG_INFINITY,
                f64::NEG_INFINITY,
            ),
            |(min_x, min_y, max_x, max_y), &(x, y)| {
                (min_x.min(x), min_y.min(y), max_x.max(x), max_y.max(y))
            },
        )
    }
}

/// Base CTM for a page: MediaBox-origin normalization composed with the
/// /Rotate transform, plus the resulting (visual) page height. Rotation
/// maps content into an upright page of swapped dimensions, so the whole
/// downstream pipeline sees a normal page.
pub(crate) fn rotation_base(
    rotate: Option<f64>,
    mb: &[f64],
    mx0: f64,
    my0: f64,
) -> (Mat, f64, f64) {
    let w = (mb[2] - mb[0]).abs();
    let h = (mb[3] - mb[1]).abs();
    let t = Mat {
        a: 1.0,
        b: 0.0,
        c: 0.0,
        d: 1.0,
        e: -mx0,
        f: -my0,
    };
    let r = ((rotate.unwrap_or(0.0) as i64 % 360) + 360) % 360;
    match r {
        90 => (
            // (x,y) → (y, w−x): 90° clockwise display; page dims swap.
            t.mul(Mat {
                a: 0.0,
                b: -1.0,
                c: 1.0,
                d: 0.0,
                e: 0.0,
                f: w,
            }),
            h,
            w,
        ),
        180 => (
            t.mul(Mat {
                a: -1.0,
                b: 0.0,
                c: 0.0,
                d: -1.0,
                e: w,
                f: h,
            }),
            w,
            h,
        ),
        270 => (
            t.mul(Mat {
                a: 0.0,
                b: 1.0,
                c: -1.0,
                d: 0.0,
                e: h,
                f: 0.0,
            }),
            h,
            w,
        ),
        _ => (t, w, h),
    }
}
