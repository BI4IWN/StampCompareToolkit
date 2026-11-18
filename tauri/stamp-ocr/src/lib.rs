//! stamp-ocr: 极坐标展开 + 切分 + PP-OCRv4 rec/cls（tract 推理），给 Tauri 外壳进程内调用。
use anyhow::{anyhow, Result};
use image::{imageops, GrayImage, RgbImage, RgbaImage};
use std::path::Path;
use tract_onnx::prelude::*;

pub struct Engine {
    rec: Model,
    cls: Option<Model>,
    dict: Vec<String>,
}

pub struct Recog {
    pub text: String,
    pub texts: Vec<String>,
    pub company: Option<String>,
    pub code: Option<String>,
    pub kind: Option<String>,
    pub warp_ratio: Option<f64>,
    pub radius: f64,
    pub channel: String,
    pub elapsed_ms: u128,
}

impl Engine {
    /// dir 里需要 rec_batch1.onnx / cls_static.onnx / dict.txt
    pub fn load(dir: &Path) -> Result<Self> {
        let rec = load(&dir.join("rec_batch1.onnx"), [1, 3, 48, 320])?;
        let cls = load(&dir.join("cls_static.onnx"), [1, 3, 48, 192]).ok();
        let dict = std::fs::read_to_string(dir.join("dict.txt"))?
            .lines().map(|s| s.to_string()).collect();
        Ok(Self { rec, cls, dict })
    }

    /// 双通道策略：先按 auto（红章 r-g / 灰章暗度）跑；若识别出的汉字 <4，再强制走 dark 通道重跑，
    /// 取汉字更多的那一次。与 08-ocr-spike/stamp_pipeline.rs 的 0.796 口径一致。
    pub fn recognize(&self, rgba: &RgbaImage) -> Result<Recog> {
        let first = self.recognize_ch(rgba, None)?;
        let cjk = |t: &str| t.chars().filter(|c| (*c as u32) >= 0x4e00).count();
        if cjk(&first.text) >= 4 {
            return Ok(first);
        }
        let second = self.recognize_ch(rgba, Some("dark"))?;
        Ok(if cjk(&second.text) > cjk(&first.text) { second } else { first })
    }

    fn recognize_ch(&self, rgba: &RgbaImage, forced: Option<&str>) -> Result<Recog> {
        let t0 = std::time::Instant::now();
        let mut rgb = RgbImage::new(rgba.width(), rgba.height());
        for (x, y, p) in rgba.enumerate_pixels() {
            // 透明处按白纸补，避免半透明边被当成墨
            let a = p.0[3] as f32 / 255.0;
            let mix = |c: u8| (c as f32 * a + 255.0 * (1.0 - a)) as u8;
            rgb.put_pixel(x, y, image::Rgb([mix(p.0[0]), mix(p.0[1]), mix(p.0[2])]));
        }
        let (strength, _) = ink_strength_ch(&rgb, forced);
        // 几何：先在 alpha 上做直线抑制 + 拟合/去畸变
        let mut alpha = alpha_of(rgba);
        suppress_lines(&mut alpha, rgba.width() as usize, rgba.height() as usize, 0.18);
        let geo = ransac_circle(&alpha, rgba.width() as usize, rgba.height() as usize)
            .map(|(cx, cy, r, _ratio, _cov)| (cx, cy, r));
        let warped = stamp_core::dewarp::dewarp_iterative(rgba);
        let (img, warp_ratio, radius) = match &warped {
            Some(d) => (d.image.clone(), Some(d.before.ratio), d.radius),
            None => (rgba.clone(), None, geo.map(|g| g.2).unwrap_or(0.0)),
        };
        let bands = radial_bands(&img, &strength, geo).map(|b| b.3).unwrap_or_default();
        let mut best: Option<(String, f32, usize)> = None;
        for (r0, r1) in bands.iter().copied().chain(std::iter::once((BAND_R0, BAND_R1))) {
            let Some(strip) = unwrap_ring_band(&img, &strength, r0, r1, geo) else { continue };
            let segs = cut_strips(&strip, 4);
            let mut texts = Vec::new();
            let mut conf = 0f32;
            for seg in segs {
                match rec_text(&self.rec, &self.dict, &seg) {
                    Ok(t) => { conf += 1.0; texts.push(t); }
                    Err(_) => texts.push(String::new()),
                }
            }
            let joined = texts.concat();
            let cjk = joined.chars().filter(|c| (*c as u32) >= 0x4e00).count();
            let score = conf * (cjk as f32).sqrt();
            if best.as_ref().map(|b| score > b.1).unwrap_or(true) {
                best = Some((joined, score, cjk));
            }
        }
        let text = best.map(|b| b.0).unwrap_or_default();
        let (company, code, kind) = postprocess(&text);
        Ok(Recog {
            texts: vec![text.clone()],
            text,
            company: if company.is_empty() { None } else { Some(company) },
            code: if code.is_empty() { None } else { Some(code) },
            kind: if kind.is_empty() { None } else { Some(kind) },
            warp_ratio,
            radius,
            channel: forced.unwrap_or("auto").to_string(),
            elapsed_ms: t0.elapsed().as_millis(),
        })
    }
}

const REC_H: usize = 48;
const REC_W: usize = 320;
const STRIP_W: usize = 2000;
const STRIP_H: usize = 120;
const BAND_R0: f64 = 0.42;
const BAND_R1: f64 = 0.905;

type Model = SimplePlan<TypedFact, Box<dyn TypedOp>, TypedModel>;

fn load(path: &Path, shape: [usize; 4]) -> Result<Model> {
    Ok(tract_onnx::onnx().model_for_path(path)?
        .with_input_fact(0, f32::fact(shape).into())?
        .into_optimized()?.into_runnable()?)
}

/// 取墨：红章优先，红像素太少则退化为「暗色墨」
/// 墨强度（0-255）：红章取 r-g，灰章取 255-min(rgb)

fn ink_strength_ch(img: &RgbImage, forced: Option<&str>) -> (Vec<u8>, bool) {
    let mut n_red = 0usize;
    let total = (img.width() * img.height()) as f64;
    for px in img.pixels() {
        let (r, g, b) = (px.0[0] as i32, px.0[1] as i32, px.0[2] as i32);
        if r > 90 && r - g > 40 && r - b > 40 { n_red += 1; }
    }
    let use_red = n_red as f64 > total * 0.002;
    let mut ch = match forced { Some(c) => c.to_string(), None => std::env::var("OCR_CH").unwrap_or_else(|_| "auto".into()) };
    if ch == "auto" {
        // 红章：r-g 的强信号足够；淡章/灰章：暗度通道恢复更好
        let mut hist = vec![0usize; 256];
        let mut n = 0usize;
        for px in img.pixels() {
            let (r, g) = (px.0[0] as i32, px.0[1] as i32);
            let v = (r - g).max(0).min(255) as usize;
            if v > 0 { hist[v] += 1; n += 1; }
        }
        let mut acc = 0usize; let mut p99 = 0usize;
        for v in (0..256).rev() { acc += hist[v]; if n > 0 && acc as f64 >= n as f64 * 0.01 { p99 = v; break; } }
        ch = if use_red && p99 >= 60 { "rg".into() } else { "dark".into() };
    }
    let mut out = vec![0u8; (img.width() * img.height()) as usize];
    for (i, px) in img.pixels().enumerate() {
        let (r, g, b) = (px.0[0] as i32, px.0[1] as i32, px.0[2] as i32);
        let rg = (r - g).max(0).min(255) as f64;
        let mx = r.max(g).max(b) as f64;
        let mn = r.min(g).min(b) as f64;
        let sat = if mx > 1.0 { ((mx - mn) / mx) * 255.0 } else { 0.0 };
        let dark = (255.0 - mn).max(0.0);
        let v = match ch.as_str() {
            "rg" => if use_red { rg } else { dark },
            "sat" => sat,
            "dark" => dark,
            // mix：彩度与红绿差取大者；灰章仍走暗度
            _ => if use_red { rg.max(sat * 0.9) } else { dark },
        };
        out[i] = v.clamp(0.0, 255.0) as u8;
    }
    // 只在"整体很暗"时增益：p99 低说明章墨本来就很淡，拉伸才有意义；
    // 正常章（p99 已经很高）拉伸只会放大扫描噪声
    let mode = std::env::var("OCR_GAIN").unwrap_or_else(|_| "auto".into());
    // 增益归一：p99 拉伸到 255；极淡章再补 gamma，避免"几乎全黑"的条带
    let mut hist = vec![0usize; 256];
    for v in &out { hist[*v as usize] += 1; }
    let total: usize = out.len();
    let mut acc = 0usize; let mut p99 = 255u8;
    for v in 0..256 { acc += hist[v]; if acc as f64 >= total as f64 * 0.99 { p99 = v as u8; break; } }
    let want = match mode.as_str() {
        "0" => false,
        "1" => true,
        _ => p99 < 60,
    };
    if want && p99 > 8 && p99 < 250 {
        let k = 255.0 / p99 as f64;
        let gamma = if (p99 as f64) < 40.0 { 0.75 } else { 1.0 };
        for v in out.iter_mut() {
            let x = (*v as f64 * k / 255.0).clamp(0.0, 1.0);
            *v = (x.powf(gamma) * 255.0).round() as u8;
        }
    }
    (out, use_red)
}


fn alpha_of(img: &RgbaImage) -> Vec<u8> { img.pixels().map(|p| p.0[3]).collect() }

/// 极坐标展开：alpha 作为墨强度，展成 STRIP_H×STRIP_W，并上下翻转成正常阅读方向

fn unwrap_ring_band(img: &RgbaImage, strength: &[u8], r0: f64, r1: f64, geo: Option<(f64, f64, f64)>) -> Option<image::GrayImage> {
    let (w, h) = (img.width() as usize, img.height() as usize);
    let alpha = alpha_of(img);
    let (cx0, cy0) = stamp_core::fit::mask_centroid(w, h, &alpha);
    let mut cx = cx0; let mut cy = cy0;
    let mut r = 0.0f64;
    let mut r_fixed: Option<f64> = None;
    if let Some((gx, gy, gr)) = geo { cx = gx; cy = gy; if gr > 1.0 { r_fixed = Some(gr); } }
    let refine_rounds: usize = std::env::var("OCR_REFINE").ok().and_then(|v| v.parse().ok()).unwrap_or(1);
    for _round in 0..refine_rounds {
        let mut d: Vec<f64> = Vec::new();
        for y in 0..h { for x in 0..w {
            if alpha[y * w + x] > 40 {
                d.push(((x as f64 - cx).powi(2) + (y as f64 - cy).powi(2)).sqrt());
            }
        }}
        if d.len() < 100 { return None; }
        d.sort_by(|a, b| a.partial_cmp(b).unwrap());
        r = r_fixed.unwrap_or(d[(d.len() as f64 * 0.992) as usize]);
        if r < 20.0 { return None; }
        // 只保留环带附近的墨迹，重算圆心（把散落在页面上的墨点排掉）
        let (mut sx, mut sy, mut n) = (0.0f64, 0.0f64, 0usize);
        for y in 0..h {
            for x in 0..w {
                if alpha[y * w + x] <= 40 { continue; }
                let dd = ((x as f64 - cx).powi(2) + (y as f64 - cy).powi(2)).sqrt();
                if dd >= 0.72 * r && dd <= 1.06 * r { sx += x as f64; sy += y as f64; n += 1; }
            }
        }
        if n < 60 || refine_rounds <= 1 || r_fixed.is_some() { break; }
        cx = sx / n as f64; cy = sy / n as f64;
    }
    let mut strip = image::GrayImage::new(STRIP_W as u32, STRIP_H as u32);
    for i in 0..STRIP_W {
        let ang = std::f64::consts::TAU * (i as f64) / (STRIP_W as f64);
        for j in 0..STRIP_H {
            let rad = r * (r0 + (r1 - r0) * (j as f64) / ((STRIP_H - 1) as f64));
            let sx = cx + rad * ang.cos();
            let sy = cy + rad * ang.sin();
            let (x0, y0) = (sx.floor() as isize, sy.floor() as isize);
            if x0 < 0 || y0 < 0 || x0 + 1 >= w as isize || y0 + 1 >= h as isize { continue; }
            let (fx, fy) = (sx - x0 as f64, sy - y0 as f64);
            let at = |x: isize, y: isize| strength[y as usize * w + x as usize] as f64;
            let v = at(x0, y0) * (1.0 - fx) * (1.0 - fy) + at(x0 + 1, y0) * fx * (1.0 - fy)
                + at(x0, y0 + 1) * (1.0 - fx) * fy + at(x0 + 1, y0 + 1) * fx * fy;
            // 展开后文字是倒的：写到翻转后的行
            let row = (STRIP_H - 1 - j) as u32;
            strip.put_pixel(i as u32, row, image::Luma([v.clamp(0.0, 255.0) as u8]));
        }
    }
    Some(strip)
}

/// 径向墨量剖面：返回 (中心x, 中心y, R, 候选环带列表)

fn radial_bands(img: &RgbaImage, strength: &[u8], geo: Option<(f64, f64, f64)>) -> Option<(f64, f64, f64, Vec<(f64, f64)>)> {
    let (w, h) = (img.width() as usize, img.height() as usize);
    let alpha = alpha_of(img);
    let (mut cx, mut cy) = stamp_core::fit::mask_centroid(w, h, &alpha);
    let mut dmax = 0f64;
    let mut acc: Vec<f64> = vec![0.0; 200];
    let mut cnt: Vec<f64> = vec![0.0; 200];
    for y in 0..h {
        for x in 0..w {
            let d = ((x as f64 - cx).powi(2) + (y as f64 - cy).powi(2)).sqrt();
            if d > dmax { dmax = d; }
            let b = (d / (h.max(w) as f64) * 200.0) as usize;
            if b < 200 {
                acc[b] += strength[y * w + x] as f64;
                cnt[b] += 1.0;
            }
        }
    }
    if let Some((gx, gy, gr)) = geo { cx = gx; cy = gy; if gr > 1.0 { dmax = gr; } }
    if dmax < 20.0 { return None; }
    let prof: Vec<f64> = (0..200).map(|b| if cnt[b] > 0.0 { acc[b] / cnt[b] } else { 0.0 }).collect();
    // 平滑
    let sm: Vec<f64> = (0usize..200).map(|i| {
        let lo = i.saturating_sub(2); let hi = (i + 3).min(200);
        prof[lo..hi].iter().sum::<f64>() / (hi - lo) as f64
    }).collect();
    let rnorm = dmax / (h.max(w) as f64) * 200.0;   // dmax 对应的 bin
    let ring_bin = (rnorm as usize).saturating_sub(3);
    // 在 [0.25R, 0.95R] 内找墨量最大的连续区间（宽度约 30 bins）
    let lo = (rnorm * 0.25) as usize;
    let hi = ring_bin.max(lo + 10);
    let mut best = (lo, hi, 0f64);
    let win = (hi - lo).min(45).max(10);
    for start in lo..=hi.saturating_sub(4) {
        let end = (start + win).min(hi);
        let v: f64 = sm[start..end].iter().sum::<f64>();
        if v > best.2 { best = (start, end, v); }
    }
    if best.2 <= 0.0 { return None; }
    let scale = (h.max(w) as f64) / 200.0;
    let bands = vec![(best.0 as f64 * scale / dmax, best.1 as f64 * scale / dmax)];
    Some((cx, cy, dmax, bands))
}


/// 抑制长直线（表格线 / 正文行）：按行、按列找超长连续墨迹段，整段置零。

fn suppress_lines(alpha: &mut [u8], w: usize, h: usize, frac: f64) {
    let min_len_r = ((w as f64) * frac) as usize;
    let min_len_c = ((h as f64) * frac) as usize;
    for y in 0..h {
        let mut x = 0usize;
        while x < w {
            if alpha[y * w + x] <= 40 { x += 1; continue; }
            let start = x;
            while x < w && alpha[y * w + x] > 40 { x += 1; }
            if x - start >= min_len_r { for i in start..x { alpha[y * w + i] = 0; } }
        }
    }
    for x in 0..w {
        let mut y = 0usize;
        while y < h {
            if alpha[y * w + x] <= 40 { y += 1; continue; }
            let start = y;
            while y < h && alpha[y * w + x] > 40 { y += 1; }
            if y - start >= min_len_c { for i in start..y { alpha[i * w + x] = 0; } }
        }
    }
}

/// RANSAC 圆拟合：印章外圈是强结构，散落墨点/正文不会形成一致圆。

fn ransac_circle(alpha: &[u8], w: usize, h: usize) -> Option<(f64, f64, f64, f64, f64)> {
    let mut pts: Vec<(f64, f64)> = Vec::new();
    let step = if w * h > 900 * 900 { 3 } else { 2 };
    for y in (0..h).step_by(step) {
        for x in (0..w).step_by(step) {
            if alpha[y * w + x] > 40 { pts.push((x as f64, y as f64)); }
        }
    }
    if pts.len() < 200 { return None; }
    let dmin = w.min(h) as f64;
    let (r_lo, r_hi) = (0.18 * dmin, 0.60 * dmin);
    let tol = (0.006 * dmin).clamp(2.0, 6.0);
    let mut best: Option<(f64, f64, f64, usize)> = None;
    let mut seed = 12345u64;
    for _ in 0..600 {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        let i1 = ((seed >> 33) as usize) % pts.len();
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        let i2 = ((seed >> 33) as usize) % pts.len();
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        let i3 = ((seed >> 33) as usize) % pts.len();
        if i1 == i2 || i2 == i3 || i1 == i3 { continue; }
        let (x1, y1) = pts[i1]; let (x2, y2) = pts[i2]; let (x3, y3) = pts[i3];
        let d = 2.0 * (x1 * (y2 - y3) + x2 * (y3 - y1) + x3 * (y1 - y2));
        if d.abs() < 1e-6 { continue; }
        let ux = ((x1 * x1 + y1 * y1) * (y2 - y3) + (x2 * x2 + y2 * y2) * (y3 - y1) + (x3 * x3 + y3 * y3) * (y1 - y2)) / d;
        let uy = ((x1 * x1 + y1 * y1) * (x3 - x2) + (x2 * x2 + y2 * y2) * (x1 - x3) + (x3 * x3 + y3 * y3) * (x2 - x1)) / d;
        let r = ((x1 - ux).powi(2) + (y1 - uy).powi(2)).sqrt();
        if r < r_lo || r > r_hi { continue; }
        let mut n = 0usize;
        for (x, y) in pts.iter() {
            let dd = ((x - ux).powi(2) + (y - uy).powi(2)).sqrt();
            if (dd - r).abs() <= tol { n += 1; }
        }
        if best.map(|(_, _, _, bn)| n > bn).unwrap_or(true) { best = Some((ux, uy, r, n)); }
    }
    let (mut cx, mut cy, mut r, bn) = best?;
    let ratio0 = bn as f64 / pts.len() as f64;
    let inliers: Vec<(f64, f64)> = pts.iter().copied()
        .filter(|(x, y)| (((x - cx).powi(2) + (y - cy).powi(2)).sqrt() - r).abs() <= tol * 1.5)
        .collect();
    if inliers.len() >= 60 {
        let n = inliers.len() as f64;
        let (mut sx, mut sy, mut sxx, mut syy, mut sxy, mut sxz, mut syz, mut sz) = (0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
        for (x, y) in &inliers {
            let z = x * x + y * y;
            sx += x; sy += y; sxx += x * x; syy += y * y; sxy += x * y; sxz += x * z; syz += y * z; sz += z;
        }
        let (a11, a12, a13) = (sxx, sxy, sx);
        let (a21, a22, a23) = (sxy, syy, sy);
        let (a31, a32, a33) = (sx, sy, n);
        let det = a11 * (a22 * a33 - a23 * a32) - a12 * (a21 * a33 - a23 * a31) + a13 * (a21 * a32 - a22 * a31);
        if det.abs() > 1e-6 {
            let (b1, b2, b3) = (sxz, syz, sz);
            let a = (b1 * (a22 * a33 - a23 * a32) - a12 * (b2 * a33 - a23 * b3) + a13 * (b2 * a32 - a22 * b3)) / det;
            let b = (a11 * (b2 * a33 - a23 * b3) - b1 * (a21 * a33 - a23 * a31) + a13 * (a21 * b3 - b2 * a31)) / det;
            let c = (a11 * (a22 * b3 - b2 * a32) - a12 * (a21 * b3 - b2 * a31) + b1 * (a21 * a32 - a22 * a31)) / det;
            let ncx = a / 2.0; let ncy = b / 2.0;
            let nr2 = c + ncx * ncx + ncy * ncy;
            if nr2 > 0.0 {
                let nr = nr2.sqrt();
                if nr > r_lo && nr < r_hi { cx = ncx; cy = ncy; r = nr; }
            }
        }
    }
    let mut bins = [false; 72];
    let mut nin = 0usize;
    for (x, y) in &inliers {
        let dd = ((x - cx).powi(2) + (y - cy).powi(2)).sqrt();
        if (dd - r).abs() <= tol * 1.5 {
            nin += 1;
            let mut ang = (y - cy).atan2(x - cx).to_degrees();
            if ang < 0.0 { ang += 360.0; }
            bins[((ang / 5.0) as usize).min(71)] = true;
        }
    }
    let coverage = bins.iter().filter(|b| **b).count() as f64 / 72.0;
    let ratio = (nin as f64 / pts.len() as f64).max(ratio0).min(1.0);
    Some((cx, cy, r, ratio, coverage))
}


fn strip_stretch_global(strip: &mut image::GrayImage) {
    let w = strip.width() as usize; let h = strip.height() as usize;
    let mut hist = vec![0usize; 256];
    for y in 0..h { for x in 0..w { hist[strip.get_pixel(x as u32, y as u32).0[0] as usize] += 1; } }
    let total: usize = hist.iter().sum();
    let mut acc = 0usize; let mut lo = 0u8; let mut hi = 255u8;
    for v in 0..256 { acc += hist[v]; if acc as f64 >= total as f64 * 0.02 { lo = v as u8; break; } }
    acc = 0;
    for v in (0..256).rev() { acc += hist[v]; if acc as f64 >= total as f64 * 0.02 { hi = v as u8; break; } }
    if hi <= lo + 8 { return; }
    let span = (hi - lo) as f64;
    for y in 0..h { for x in 0..w {
        let v = strip.get_pixel(x as u32, y as u32).0[0] as f64;
        let nv = (((v - lo as f64) / span).clamp(0.0, 1.0) * 255.0) as u8;
        strip.put_pixel(x as u32, y as u32, image::Luma([nv]));
    }}
}


fn strip_normalize(strip: &mut image::GrayImage) {
    // 每 250 列一块：减去块内中位数，再按块内 95 分位拉伸
    let w = strip.width() as usize;
    let h = strip.height() as usize;
    let mut vals: Vec<u8> = (0..w * h).map(|i| strip.get_pixel((i % w) as u32, (i / w) as u32).0[0]).collect();
    let block = 250usize;
    for bs in (0..w).step_by(block) {
        let be = (bs + block).min(w);
        let mut samples: Vec<u8> = Vec::new();
        for x in bs..be { for y in 0..h { samples.push(vals[y * w + x]); } }
        if samples.is_empty() { continue; }
        samples.sort_unstable();
        let med = samples[samples.len() / 2] as f64;
        let p95 = samples[(samples.len() as f64 * 0.95) as usize] as f64;
        let span = (p95 - med).max(12.0);
        for x in bs..be {
            for y in 0..h {
                let v = vals[y * w + x] as f64;
                let nv = ((v - med) / span * 255.0).clamp(0.0, 255.0);
                vals[y * w + x] = nv as u8;
            }
        }
    }
    for y in 0..h { for x in 0..w { strip.put_pixel(x as u32, y as u32, image::Luma([vals[y * w + x]])); } }
}


fn strip_to_rgb(s: &image::GrayImage) -> RgbImage {
    let mut out = RgbImage::new(s.width(), s.height());
    for (x, y, p) in s.enumerate_pixels() {
        out.put_pixel(x, y, image::Rgb([p.0[0], p.0[0], p.0[0]]));
    }
    out
}

/// 批量识别：把多张裁剪图拼成一个 [N,3,48,320] 张量一次前向

fn rotate180(img: &RgbImage) -> RgbImage {
    let (w, h) = (img.width(), img.height());
    let mut o = RgbImage::new(w, h);
    for y in 0..h { for x in 0..w { o.put_pixel(w - 1 - x, h - 1 - y, *img.get_pixel(x, y)); } }
    o
}

/// 文字行方向分类（0=正，1=倒）；返回 (是否需翻转, 置信度)

fn need_rotate(cls: &Model, img: &RgbImage) -> Result<(bool, f32)> {
    let rs = imageops::resize(img, 192, 48, imageops::FilterType::Triangle);
    let mut data = vec![0f32; 3 * 48 * 192];
    for y in 0..48 {
        for x in 0..192 {
            let px = rs.get_pixel(x as u32, y as u32).0;
            for c in 0..3 { data[c * 48 * 192 + y * 192 + x] = (px[c] as f32 / 255.0 - 0.5) / 0.5; }
        }
    }
    let input = tract_ndarray::Array4::from_shape_vec((1, 3, 48, 192), data)?;
    let out = cls.run(tvec!(input.into_tensor().into()))?;
    let o = out[0].to_array_view::<f32>()?;
    let (a, b) = (o[[0, 0]], o[[0, 1]]);
    Ok((b > a, a.max(b)))
}


fn to_halfwidth(c: char) -> char {
    match c {
        '０'..='９' => char::from_u32(c as u32 - 0xFF10 + '0' as u32).unwrap_or(c),
        'Ａ'..='Ｚ' => char::from_u32(c as u32 - 0xFF21 + 'A' as u32).unwrap_or(c),
        'ａ'..='ｚ' => char::from_u32(c as u32 - 0xFF41 + 'a' as u32).unwrap_or(c),
        _ => c,
    }
}


fn normalize_text(t: &str) -> String {
    t.chars().map(to_halfwidth)
        .filter(|c| c.is_ascii_alphanumeric() || ('\u{4e00}'..='\u{9fff}').contains(c) || *c == '(' || *c == ')' || *c == '-')
        .collect()
}


const ORG_SUFFIX: &[&str] = &[
    "股份有限公司", "有限责任公司", "有限公司", "分公司", "委员会办公室", "委员会",
    "管理局", "公安局", "财政局", "银行", "支队", "部队", "分局", "集团", "公司", "局", "厅", "院", "所",
];

/// 结果后处理：拆出 (机构名, 编码, 类型)

fn postprocess(text: &str) -> (String, String, String) {
    let norm = normalize_text(text);
    let mut best_code = String::new();
    let mut run = String::new();
    for c in norm.chars() {
        if c.is_ascii_digit() { run.push(c); }
        else { if run.len() > best_code.len() { best_code = run.clone(); } run.clear(); }
    }
    if run.len() > best_code.len() { best_code = run; }
    if best_code.len() < 8 { best_code.clear(); }

    let chars: Vec<char> = norm.chars().collect();
    let mut best_org = String::new();
    let mut best_type = String::new();
    let mut best_len = 0usize;
    for (i, _) in chars.iter().enumerate() {
        for suf in ORG_SUFFIX {
            let sc: Vec<char> = suf.chars().collect();
            if i + sc.len() > chars.len() { continue; }
            if chars[i..i + sc.len()] != sc[..] { continue; }
            let mut start = i;
            while start > 0 && ('\u{4e00}'..='\u{9fff}').contains(&chars[start - 1]) { start -= 1; }
            let cand: String = chars[start..i + sc.len()].iter().collect();
            let l = cand.chars().count();
            if l > best_len { best_len = l; best_org = cand; best_type = suf.to_string(); }
        }
    }
    (best_org, best_code, best_type)
}


fn rec_batch(model: &Model, dict: &[String], imgs: &[RgbImage], batch: usize) -> Result<Vec<(String, f32)>> {
    let mut out_texts: Vec<(String, f32)> = Vec::with_capacity(imgs.len());
    for chunk in imgs.chunks(batch) {
        let mut data = vec![0f32; batch * 3 * REC_H * REC_W];
        for (bi, img) in chunk.iter().enumerate() {
            let (w, h) = (img.width() as f32, img.height() as f32);
            if w < 2.0 || h < 2.0 { continue; }
            let rw = ((REC_H as f32 * (w / h)).ceil() as usize).clamp(4, REC_W);
            let rs = imageops::resize(img, rw as u32, REC_H as u32, imageops::FilterType::Triangle);
            for y in 0..REC_H {
                for x in 0..rw {
                    let px = rs.get_pixel(x as u32, y as u32).0;
                    for c in 0..3 {
                        let idx = ((bi * 3 + c) * REC_H + y) * REC_W + x;
                        data[idx] = (px[c] as f32 / 255.0 - 0.5) / 0.5;
                    }
                }
            }
        }
        let input = tract_ndarray::Array4::from_shape_vec((batch, 3, REC_H, REC_W), data)?;
        let out = model.run(tvec!(input.into_tensor().into()))?;
        let o = out[0].to_array_view::<f32>()?;
        let (steps, classes) = (o.shape()[1], o.shape()[2]);
        for bi in 0..batch {
            let (mut text, mut prev) = (String::new(), usize::MAX);
            let (mut sum, mut cnt) = (0f64, 0usize);
            for t in 0..steps {
                let mut best = f32::MIN; let mut bi_c = 0usize;
                for c in 0..classes { let v = o[[bi, t, c]]; if v > best { best = v; bi_c = c; } }
                if bi_c != 0 {
                    sum += best as f64; cnt += 1;
                    if bi_c != prev {
                        let idx = bi_c - 1;
                        if idx < dict.len() { text.push_str(&dict[idx]); }
                    }
                }
                prev = bi_c;
            }
            let conf = if cnt > 0 { (sum / cnt as f64) as f32 } else { 0.0 };
            out_texts.push(if bi < chunk.len() { (text, conf) } else { (String::new(), 0.0) });
        }
    }
    out_texts.truncate(imgs.len());
    Ok(out_texts)
}


fn rec_text(model: &Model, dict: &[String], img: &RgbImage) -> Result<String> {
    let (w, h) = (img.width() as f32, img.height() as f32);
    if w < 2.0 || h < 2.0 { return Ok(String::new()); }
    let rw = ((REC_H as f32 * (w / h)).ceil() as usize).clamp(4, REC_W);
    let rs = imageops::resize(img, rw as u32, REC_H as u32, imageops::FilterType::Triangle);
    let mut data = vec![0f32; 3 * REC_H * REC_W];
    for y in 0..REC_H { for x in 0..rw {
        let px = rs.get_pixel(x as u32, y as u32).0;
        for c in 0..3 { data[c * REC_H * REC_W + y * REC_W + x] = (px[c] as f32 / 255.0 - 0.5) / 0.5; }
    }}
    let input = tract_ndarray::Array4::from_shape_vec((1, 3, REC_H, REC_W), data)?;
    let out = model.run(tvec!(input.into_tensor().into()))?;
    let o = out[0].to_array_view::<f32>()?;
    let (steps, classes) = (o.shape()[1], o.shape()[2]);
    let (mut text, mut prev) = (String::new(), usize::MAX);
    for t in 0..steps {
        let mut best = f32::MIN; let mut bi = 0usize;
        for c in 0..classes { let v = o[[0, t, c]]; if v > best { best = v; bi = c; } }
        if bi != 0 && bi != prev { let idx = bi - 1; if idx < dict.len() { text.push_str(&dict[idx]); } }
        prev = bi;
    }
    Ok(text)
}


/// 把 2000×120 的条带切成 n 段，切点吸附到列投影（墨量）最小处
fn cut_strips(strip: &GrayImage, n: usize) -> Vec<RgbImage> {
    let (w, h) = (strip.width() as usize, strip.height() as usize);
    let mut col = vec![0f64; w];
    for x in 0..w {
        let mut s = 0f64;
        for y in 0..h { s += strip.get_pixel(x as u32, y as u32).0[0] as f64; }
        col[x] = s;
    }
    let mut cuts = vec![0usize];
    for k in 1..n {
        let nominal = w * k / n;
        let lo = nominal.saturating_sub(70);
        let hi = (nominal + 70).min(w - 1);
        let mut bx = nominal; let mut bv = f64::MAX;
        for x in lo..hi { if col[x] < bv { bv = col[x]; bx = x; } }
        cuts.push(bx);
    }
    cuts.push(w);
    let mut out = Vec::new();
    for k in 0..n {
        let (a, b) = (cuts[k], cuts[k + 1]);
        if b <= a { continue; }
        let sub = image::imageops::crop_imm(strip, a as u32, 0, (b - a) as u32, h as u32).to_image();
        out.push(strip_to_rgb(&sub));
    }
    out
}
