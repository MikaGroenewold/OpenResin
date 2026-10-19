use super::{RasterOptions, Segment};

/// A run of `len` identical greyscale pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Run {
    pub value: u8,
    pub len: u32,
}

/// A layer image stored as row-major run-length encoded greyscale pixels.
/// Runs may span row boundaries; their lengths always add up to `width * height`.
#[derive(Debug, Clone, PartialEq)]
pub struct LayerRaster {
    pub width: u32,
    pub height: u32,
    pub runs: Vec<Run>,
    /// Bounding box of lit pixels as `(x0, y0, x1, y1)` with exclusive max.
    pub bbox: Option<(u32, u32, u32, u32)>,
    lit: f64,
}

impl LayerRaster {
    pub fn empty(width: u32, height: u32) -> Self {
        let mut b = Builder::new(width, height);
        b.push_raw(0, width * height);
        b.finish()
    }

    /// Builds a raster from a full greyscale image.
    pub fn from_gray(width: u32, height: u32, pixels: &[u8]) -> Self {
        assert_eq!(pixels.len(), (width * height) as usize);
        let mut b = Builder::new(width, height);
        for row in pixels.chunks_exact(width as usize) {
            let mut i = 0;
            while i < row.len() {
                let v = row[i];
                let mut j = i + 1;
                while j < row.len() && row[j] == v {
                    j += 1;
                }
                b.emit(v, (j - i) as u32);
                i = j;
            }
        }
        b.finish()
    }

    /// Builds a raster from runs (e.g. decoded from a file).
    pub fn from_runs(width: u32, height: u32, runs: impl IntoIterator<Item = Run>) -> Self {
        let mut b = Builder::new(width, height);
        let total = width as u64 * height as u64;
        let mut written = 0u64;
        for r in runs {
            let len = (r.len as u64).min(total - written) as u32;
            // split at row boundaries so the bbox stays exact
            let mut left = len;
            while left > 0 {
                let in_row = (width - b.x).min(left);
                b.emit(r.value, in_row);
                left -= in_row;
            }
            written += len as u64;
            if written >= total {
                break;
            }
        }
        if written < total {
            let mut left = (total - written) as u32;
            while left > 0 {
                let in_row = (width - b.x).min(left);
                b.emit(0, in_row);
                left -= in_row;
            }
        }
        b.finish()
    }

    pub fn pixel_count(&self) -> u64 {
        self.runs.iter().map(|r| r.len as u64).sum()
    }

    /// Number of fully lit pixel equivalents (sum of value / 255).
    pub fn lit_area_px(&self) -> f64 {
        self.lit
    }

    pub fn is_blank(&self) -> bool {
        self.bbox.is_none()
    }

    pub fn to_gray(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity((self.width * self.height) as usize);
        for r in &self.runs {
            out.extend(std::iter::repeat_n(r.value, r.len as usize));
        }
        out
    }

    /// Calls `f(y, row)` for every row with the decoded pixels of that row.
    pub fn for_each_row(&self, mut f: impl FnMut(u32, &[u8])) {
        let w = self.width as usize;
        let mut row = vec![0u8; w];
        let mut x = 0usize;
        let mut y = 0u32;
        for r in &self.runs {
            let mut left = r.len as usize;
            while left > 0 {
                let n = (w - x).min(left);
                row[x..x + n].fill(r.value);
                x += n;
                left -= n;
                if x == w {
                    f(y, &row);
                    y += 1;
                    x = 0;
                }
            }
        }
    }

    /// Box-filtered downscale so that the result fits into `max_w` x `max_h`.
    /// Returns `(width, height, pixels)`.
    pub fn downscale(&self, max_w: u32, max_h: u32) -> (u32, u32, Vec<u8>) {
        let f = (self.width.div_ceil(max_w.max(1)))
            .max(self.height.div_ceil(max_h.max(1)))
            .max(1);
        let ow = self.width.div_ceil(f);
        let oh = self.height.div_ceil(f);
        let mut acc = vec![0u32; (ow * oh) as usize];
        let (mut x, mut y) = (0u32, 0u32);
        for r in &self.runs {
            let mut left = r.len;
            while left > 0 {
                let n = (self.width - x).min(left);
                if r.value != 0 {
                    let oy = (y / f) * ow;
                    let mut xx = x;
                    let end = x + n;
                    while xx < end {
                        let ox = xx / f;
                        let cell_end = ((ox + 1) * f).min(end);
                        acc[(oy + ox) as usize] += r.value as u32 * (cell_end - xx);
                        xx = cell_end;
                    }
                }
                x += n;
                left -= n;
                if x == self.width {
                    x = 0;
                    y += 1;
                }
            }
        }
        let div = f * f;
        let pixels = acc.into_iter().map(|v| (v / div).min(255) as u8).collect();
        (ow, oh, pixels)
    }
}

/// Incrementally builds a [`LayerRaster`] row by row.
pub(crate) struct Builder {
    width: u32,
    height: u32,
    runs: Vec<Run>,
    x: u32,
    y: u32,
    bbox: Option<(u32, u32, u32, u32)>,
    lit: f64,
}

impl Builder {
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            runs: Vec::new(),
            x: 0,
            y: 0,
            bbox: None,
            lit: 0.0,
        }
    }

    #[inline]
    fn push_raw(&mut self, value: u8, len: u32) {
        if len == 0 {
            return;
        }
        match self.runs.last_mut() {
            Some(last) if last.value == value => last.len += len,
            _ => self.runs.push(Run { value, len }),
        }
    }

    /// Emits pixels within the current row (must not cross the row end).
    #[inline]
    pub fn emit(&mut self, value: u8, len: u32) {
        if len == 0 {
            return;
        }
        debug_assert!(self.x + len <= self.width);
        if value != 0 {
            self.lit += value as f64 / 255.0 * len as f64;
            let (x0, x1, y) = (self.x, self.x + len, self.y);
            self.bbox = Some(match self.bbox {
                None => (x0, y, x1, y + 1),
                Some((a, b, c, d)) => (a.min(x0), b.min(y), c.max(x1), d.max(y + 1)),
            });
        }
        self.push_raw(value, len);
        self.x += len;
        if self.x == self.width {
            self.x = 0;
            self.y += 1;
        }
    }

    /// Emits `rows` complete blank rows (current position must be at a row start).
    pub fn blank_rows(&mut self, rows: u32) {
        debug_assert_eq!(self.x, 0);
        self.push_raw(0, rows * self.width);
        self.y += rows;
    }

    pub fn finish(self) -> LayerRaster {
        LayerRaster {
            width: self.width,
            height: self.height,
            runs: self.runs,
            bbox: self.bbox,
            lit: self.lit,
        }
    }
}

struct Edge {
    ymin: f32,
    ymax: f32,
    x_at_ymin: f32,
    dxdy: f32,
    dir: i32,
}

#[inline]
fn quantize(c: f32) -> u8 {
    (c.clamp(0.0, 1.0) * 255.0 + 0.5) as u8
}

pub(crate) fn rasterize(segs: &[Segment], o: &RasterOptions) -> LayerRaster {
    let (w, h) = (o.width, o.height);
    let mut b = Builder::new(w, h);
    let mut edges: Vec<Edge> = segs
        .iter()
        .filter(|s| s.y0 != s.y1)
        .map(|s| {
            let (ya, yb, xa, xb, dir) = if s.y0 < s.y1 {
                (s.y0, s.y1, s.x0, s.x1, 1)
            } else {
                (s.y1, s.y0, s.x1, s.x0, -1)
            };
            Edge {
                ymin: ya,
                ymax: yb,
                x_at_ymin: xa,
                dxdy: (xb - xa) / (yb - ya),
                dir,
            }
        })
        .filter(|e| e.ymax > 0.0 && e.ymin < h as f32)
        .collect();
    if edges.is_empty() {
        b.blank_rows(h);
        return b.finish();
    }
    edges.sort_unstable_by(|a, b| a.ymin.total_cmp(&b.ymin));
    let row_start = (edges[0].ymin.floor().max(0.0) as u32).min(h);
    let row_end = (edges.iter().map(|e| e.ymax).fold(0f32, f32::max).ceil().max(0.0) as u32).min(h);
    b.blank_rows(row_start);

    let ss = o.anti_aliasing.max(1) as u32;
    let wf = w as f32;
    let mut active: Vec<usize> = Vec::new();
    let mut next = 0usize;
    let mut xs: Vec<(f32, i32)> = Vec::new();
    let mut spans: Vec<(f32, f32)> = Vec::new();
    // (pixel, partial coverage, change of full coverage)
    let mut events: Vec<(u32, f32, f32)> = Vec::new();

    let mut spans_at = |y: f32, active: &mut Vec<usize>, next: &mut usize, spans: &mut Vec<(f32, f32)>| {
        while *next < edges.len() && edges[*next].ymin <= y {
            active.push(*next);
            *next += 1;
        }
        active.retain(|&i| edges[i].ymax > y);
        xs.clear();
        for &i in active.iter() {
            let e = &edges[i];
            if e.ymin <= y {
                xs.push((e.x_at_ymin + (y - e.ymin) * e.dxdy, e.dir));
            }
        }
        xs.sort_unstable_by(|a, b| a.0.total_cmp(&b.0));
        spans.clear();
        let total: i32 = xs.iter().map(|x| x.1).sum();
        if total == 0 {
            let mut wnd = 0;
            let mut start = 0f32;
            for &(x, d) in xs.iter() {
                let prev = wnd;
                wnd += d;
                if prev == 0 && wnd != 0 {
                    start = x;
                } else if prev != 0 && wnd == 0 {
                    spans.push((start, x));
                }
            }
        } else {
            // Open/non-manifold geometry: fall back to even-odd pairing.
            for p in xs.chunks_exact(2) {
                spans.push((p[0].0, p[1].0));
            }
        }
    };

    for r in row_start..row_end {
        if ss == 1 {
            spans_at(r as f32 + 0.5, &mut active, &mut next, &mut spans);
            let mut cursor = 0u32;
            for &(xa, xb) in &spans {
                let s = ((xa - 0.5).ceil().clamp(0.0, wf) as u32).max(cursor);
                let e = (xb - 0.5).ceil().clamp(0.0, wf) as u32;
                if e <= s {
                    continue;
                }
                b.emit(0, s - cursor);
                b.emit(255, e - s);
                cursor = e;
            }
            b.emit(0, w - cursor);
        } else {
            events.clear();
            let wgt = 1.0 / ss as f32;
            for k in 0..ss {
                let y = r as f32 + (k as f32 + 0.5) / ss as f32;
                spans_at(y, &mut active, &mut next, &mut spans);
                for &(xa, xb) in &spans {
                    let xa = xa.max(0.0);
                    let xb = xb.min(wf);
                    if xb <= xa {
                        continue;
                    }
                    let ja = xa as u32;
                    let jb = xb as u32;
                    if ja == jb {
                        events.push((ja, (xb - xa) * wgt, 0.0));
                    } else {
                        events.push((ja, ((ja + 1) as f32 - xa) * wgt, 0.0));
                        if jb > ja + 1 {
                            events.push((ja + 1, 0.0, wgt));
                            events.push((jb, 0.0, -wgt));
                        }
                        let f = xb - jb as f32;
                        if jb < w && f > 0.0 {
                            events.push((jb, f * wgt, 0.0));
                        }
                    }
                }
            }
            events.sort_unstable_by_key(|e| e.0);
            let mut x = 0u32;
            let mut full = 0f32;
            let mut i = 0;
            while i < events.len() {
                let p = events[i].0;
                if p >= w {
                    break;
                }
                if p > x {
                    b.emit(quantize(full), p - x);
                }
                let mut partial = 0f32;
                while i < events.len() && events[i].0 == p {
                    partial += events[i].1;
                    full += events[i].2;
                    i += 1;
                }
                b.emit(quantize(full + partial), 1);
                x = p + 1;
            }
            if x < w {
                b.emit(quantize(full), w - x);
            }
        }
    }
    b.blank_rows(h - row_end);
    b.finish()
}

/// Active edge table answering "which x-intervals are inside at height y" for
/// monotonically increasing y (nonzero winding; even-odd for broken input).
struct EdgeTable {
    edges: Vec<Edge>,
    active: Vec<usize>,
    next: usize,
    xs: Vec<(f32, i32)>,
}

impl EdgeTable {
    fn new(segs: &[Segment], h: u32) -> Self {
        let mut edges: Vec<Edge> = segs
            .iter()
            .filter(|s| s.y0 != s.y1)
            .map(|s| {
                let (ya, yb, xa, xb, dir) = if s.y0 < s.y1 {
                    (s.y0, s.y1, s.x0, s.x1, 1)
                } else {
                    (s.y1, s.y0, s.x1, s.x0, -1)
                };
                Edge {
                    ymin: ya,
                    ymax: yb,
                    x_at_ymin: xa,
                    dxdy: (xb - xa) / (yb - ya),
                    dir,
                }
            })
            .filter(|e| e.ymax > 0.0 && e.ymin < h as f32)
            .collect();
        edges.sort_unstable_by(|a, b| a.ymin.total_cmp(&b.ymin));
        Self {
            edges,
            active: Vec::new(),
            next: 0,
            xs: Vec::new(),
        }
    }

    fn is_empty(&self) -> bool {
        self.edges.is_empty()
    }

    /// Row range `[start, end)` touched by the edges.
    fn rows(&self, h: u32) -> (u32, u32) {
        if self.edges.is_empty() {
            return (0, 0);
        }
        let start = (self.edges[0].ymin.floor().max(0.0) as u32).min(h);
        let end = (self.edges.iter().map(|e| e.ymax).fold(0f32, f32::max).ceil().max(0.0) as u32).min(h);
        (start, end)
    }

    fn spans_at(&mut self, y: f32, spans: &mut Vec<(f32, f32)>) {
        let edges = &self.edges;
        while self.next < edges.len() && edges[self.next].ymin <= y {
            self.active.push(self.next);
            self.next += 1;
        }
        self.active.retain(|&i| edges[i].ymax > y);
        self.xs.clear();
        for &i in self.active.iter() {
            let e = &edges[i];
            if e.ymin <= y {
                self.xs.push((e.x_at_ymin + (y - e.ymin) * e.dxdy, e.dir));
            }
        }
        self.xs.sort_unstable_by(|a, b| a.0.total_cmp(&b.0));
        spans.clear();
        let total: i32 = self.xs.iter().map(|x| x.1).sum();
        if total == 0 {
            let mut wnd = 0;
            let mut start = 0f32;
            for &(x, d) in self.xs.iter() {
                let prev = wnd;
                wnd += d;
                if prev == 0 && wnd != 0 {
                    start = x;
                } else if prev != 0 && wnd == 0 {
                    spans.push((start, x));
                }
            }
        } else {
            for p in self.xs.chunks_exact(2) {
                spans.push((p[0].0, p[1].0));
            }
        }
    }
}

/// `out = a \ b` for sorted, disjoint interval lists.
fn subtract_spans(a: &[(f32, f32)], b: &[(f32, f32)], out: &mut Vec<(f32, f32)>) {
    out.clear();
    let mut j = 0;
    for &(s0, e) in a {
        let mut s = s0;
        while j < b.len() && b[j].1 <= s {
            j += 1;
        }
        let mut k = j;
        while k < b.len() && b[k].0 < e {
            if b[k].0 > s {
                out.push((s, b[k].0));
            }
            s = s.max(b[k].1);
            if s >= e {
                break;
            }
            k += 1;
        }
        if s < e {
            out.push((s, e));
        }
    }
}

/// Rasterizes `pos` minus `neg`: a point is solid where the winding of the
/// positive outlines is non-zero and the winding of the negative outlines is
/// zero. The subtraction happens on exact span coordinates per (sub-)row, so
/// anti-aliased coverage stays exact.
pub(crate) fn rasterize_masked(pos: &[Segment], neg: &[Segment], o: &RasterOptions) -> LayerRaster {
    let (w, h) = (o.width, o.height);
    let mut pt = EdgeTable::new(pos, h);
    let mut nt = EdgeTable::new(neg, h);
    if nt.is_empty() {
        return rasterize(pos, o);
    }
    let mut b = Builder::new(w, h);
    if pt.is_empty() {
        b.blank_rows(h);
        return b.finish();
    }
    let (row_start, row_end) = pt.rows(h);
    b.blank_rows(row_start);
    let ss = o.anti_aliasing.max(1) as u32;
    let wf = w as f32;
    let mut ps: Vec<(f32, f32)> = Vec::new();
    let mut ns: Vec<(f32, f32)> = Vec::new();
    let mut spans: Vec<(f32, f32)> = Vec::new();
    let mut events: Vec<(u32, f32, f32)> = Vec::new();
    let mut spans_at = |y: f32, spans: &mut Vec<(f32, f32)>| {
        pt.spans_at(y, &mut ps);
        nt.spans_at(y, &mut ns);
        if ns.is_empty() {
            spans.clear();
            spans.extend_from_slice(&ps);
        } else {
            subtract_spans(&ps, &ns, spans);
        }
    };
    for r in row_start..row_end {
        if ss == 1 {
            spans_at(r as f32 + 0.5, &mut spans);
            let mut cursor = 0u32;
            for &(xa, xb) in &spans {
                let s = ((xa - 0.5).ceil().clamp(0.0, wf) as u32).max(cursor);
                let e = (xb - 0.5).ceil().clamp(0.0, wf) as u32;
                if e <= s {
                    continue;
                }
                b.emit(0, s - cursor);
                b.emit(255, e - s);
                cursor = e;
            }
            b.emit(0, w - cursor);
        } else {
            events.clear();
            let wgt = 1.0 / ss as f32;
            for k in 0..ss {
                let y = r as f32 + (k as f32 + 0.5) / ss as f32;
                spans_at(y, &mut spans);
                for &(xa, xb) in &spans {
                    let xa = xa.max(0.0);
                    let xb = xb.min(wf);
                    if xb <= xa {
                        continue;
                    }
                    let ja = xa as u32;
                    let jb = xb as u32;
                    if ja == jb {
                        events.push((ja, (xb - xa) * wgt, 0.0));
                    } else {
                        events.push((ja, ((ja + 1) as f32 - xa) * wgt, 0.0));
                        if jb > ja + 1 {
                            events.push((ja + 1, 0.0, wgt));
                            events.push((jb, 0.0, -wgt));
                        }
                        let f = xb - jb as f32;
                        if jb < w && f > 0.0 {
                            events.push((jb, f * wgt, 0.0));
                        }
                    }
                }
            }
            events.sort_unstable_by_key(|e| e.0);
            let mut x = 0u32;
            let mut full = 0f32;
            let mut i = 0;
            while i < events.len() {
                let p = events[i].0;
                if p >= w {
                    break;
                }
                if p > x {
                    b.emit(quantize(full), p - x);
                }
                let mut partial = 0f32;
                while i < events.len() && events[i].0 == p {
                    partial += events[i].1;
                    full += events[i].2;
                    i += 1;
                }
                b.emit(quantize(full + partial), 1);
                x = p + 1;
            }
            if x < w {
                b.emit(quantize(full), w - x);
            }
        }
    }
    b.blank_rows(h - row_end);
    b.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gray_roundtrip_and_rows() {
        let w = 7;
        let h = 3;
        let px: Vec<u8> = (0..w * h)
            .map(|i| if i % 5 == 0 { 255 } else { (i * 3) as u8 % 7 })
            .collect();
        let r = LayerRaster::from_gray(w, h, &px);
        assert_eq!(r.to_gray(), px);
        let mut rows = Vec::new();
        r.for_each_row(|_, row| rows.extend_from_slice(row));
        assert_eq!(rows, px);
        let r2 = LayerRaster::from_runs(w, h, r.runs.iter().copied());
        assert_eq!(r2.to_gray(), px);
        assert_eq!(r2.bbox, r.bbox);
    }

    #[test]
    fn span_subtraction() {
        let mut out = Vec::new();
        subtract_spans(
            &[(0.0, 10.0), (20.0, 30.0)],
            &[(2.0, 3.0), (5.0, 22.0), (25.0, 40.0)],
            &mut out,
        );
        assert_eq!(out, vec![(0.0, 2.0), (3.0, 5.0), (22.0, 25.0)]);
        subtract_spans(&[(0.0, 10.0)], &[], &mut out);
        assert_eq!(out, vec![(0.0, 10.0)]);
        subtract_spans(&[(0.0, 10.0)], &[(-5.0, 15.0)], &mut out);
        assert!(out.is_empty());
        subtract_spans(&[(0.0, 1.0), (2.0, 3.0)], &[(1.0, 2.0)], &mut out);
        assert_eq!(out, vec![(0.0, 1.0), (2.0, 3.0)]);
    }

    #[test]
    fn downscale_average() {
        let px = vec![255u8; 16];
        let r = LayerRaster::from_gray(4, 4, &px);
        let (w, h, d) = r.downscale(2, 2);
        assert_eq!((w, h), (2, 2));
        assert!(d.iter().all(|&v| v == 255));
    }
}
