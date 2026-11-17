// 整枚章 Rust 管线：取墨 → 圆章校正 → 极坐标展开 → 行切分（或 DB det）→ 识别
// 用法: stamp_pipeline <data-dir> <images-dir> [--det]
use anyhow::{Context, Result};
use image::{imageops, RgbImage, RgbaImage};
use std::path::{Path, PathBuf};
use std::time::Instant;
use tract_onnx::prelude::*;

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
fn ink_strength(img: &RgbImage) -> (Vec<u8>, bool) {
    let mut n_red = 0usize;
    let total = (img.width() * img.height()) as f64;
    for px in img.pixels() {
        let (r, g, b) = (px.0[0] as i32, px.0[1] as i32, px.0[2] as i32);
        if r > 90 && r - g > 40 && r - b > 40 { n_red += 1; }
    }
    let use_red = n_red as f64 > total * 0.002;
    let mut ch = std::env::var("OCR_CH").unwrap_or_else(|_| "auto".into());
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

fn ink_mask(img: &RgbImage) -> RgbaImage {
    let (w, h) = (img.width(), img.height());
    let mut out = RgbaImage::new(w, h);
    let mut n_red = 0usize;
    let mut red = vec![false; (w * h) as usize];
    for (i, px) in img.pixels().enumerate() {
        let (r, g, b) = (px.0[0] as i32, px.0[1] as i32, px.0[2] as i32);
        if r > 90 && r - g > 40 && r - b > 40 { red[i] = true; n_red += 1; }
    }
    let use_red = n_red as f64 > (w * h) as f64 * 0.002;
    for (i, px) in img.pixels().enumerate() {
        let (r, g, b) = (px.0[0] as i32, px.0[1] as i32, px.0[2] as i32);
        let mut keep = if use_red { red[i] } else {
            let v = (r.max(g).max(b)) as f64 / 255.0;
            let mx = r.max(g).max(b) as f64;
            let mn = r.min(g).min(b) as f64;
            let s = if mx > 0.0 { (mx - mn) / mx } else { 0.0 };
            v <= 0.62 && s <= 0.45 && v >= 0.03
        };
        // 暗色路径下，红像素也算（红色章里 r-g 大但 v 可能不低）
        if !use_red && red[i] { keep = true; }
        let (x, y) = ((i as u32) % w, (i as u32) / w);
        out.put_pixel(x, y, image::Rgba([px.0[0], px.0[1], px.0[2], if keep { 255 } else { 0 }]));
    }
    out
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

fn recall(truth: &str, pred: &str) -> f64 {
    let mut tt = std::collections::HashMap::new();
    for ch in truth.chars() { *tt.entry(ch).or_insert(0i32) += 1; }
    let mut hit = 0;
    for ch in pred.chars() { if let Some(c) = tt.get_mut(&ch) { if *c > 0 { *c -= 1; hit += 1 } } }
    hit as f64 / truth.chars().count().max(1) as f64
}

fn run_channel(rec: &Model, dict: &[String], batch: usize, img: &RgbImage, ch: &str) -> Result<(String, f32, f32, f64, f64, usize)> {
    std::env::set_var("OCR_CH", ch);
    let t0 = Instant::now();
    let masked = ink_mask(img);
    let (w, h) = (masked.width() as usize, masked.height() as usize);
    let mut alpha = alpha_of(&masked);
    let use_ransac = std::env::var("OCR_RANSAC").map(|v| v != "0").unwrap_or(true);
    let mut geo: Option<(f64, f64, f64)> = None;
    if use_ransac {
        suppress_lines(&mut alpha, w, h, 0.18);
        if let Some((cx, cy, r, ratio, cov)) = ransac_circle(&alpha, w, h) {
            if std::env::var("OCR_DEBUG").is_ok() {
                eprintln!("ransac r={:.1} ratio={:.2} cov={:.2} c=({:.0},{:.0})", r, ratio, cov, cx, cy);
            }
            let rmode = std::env::var("OCR_RMODE").unwrap_or_else(|_| "center".into());
            if ratio >= 0.30 && cov >= 0.45 {
                if rmode == "center" {
                    // 只信 RANSAC 的圆心，半径仍用分位数（保留原来的环带尺度）
                    geo = Some((cx, cy, 0.0));
                } else {
                    geo = Some((cx, cy, r));
                }
            }
        }
    }
    let mut base = masked;
    let mut warp_ratio = 1.0f64;
    if let Some(el) = stamp_core::fit::fit_ellipse(w, h, &alpha) {
        let rep = stamp_core::gates::check(w, h, &alpha, &el);
        if rep.ok {
            if let Some(dw) = stamp_core::dewarp_iterative(&base) {
                warp_ratio = dw.before.ratio / dw.after.map(|e| e.ratio).unwrap_or(dw.before.ratio).max(1e-6);
                base = dw.image;
            }
        }
    }
    let base_rgb: RgbImage = {
        let mut o = RgbImage::new(base.width(), base.height());
        for (x, y, p) in base.enumerate_pixels() { o.put_pixel(x, y, image::Rgb([p.0[0], p.0[1], p.0[2]])); }
        o
    };
    let (strength, _) = ink_strength(&base_rgb);
    // 环带：用径向剖面找文字带（避开外圈），支持 1-2 个候选；与默认带取并集
    let mut bands: Vec<(f64, f64)> = vec![(BAND_R0, BAND_R1)];
    let max_bands: usize = std::env::var("OCR_BANDS").ok().and_then(|v| v.parse().ok()).unwrap_or(2);
    if max_bands > 1 { if let Some((_cx, _cy, _r, b)) = radial_bands(&base, &strength, geo) {
        for (b0, b1) in b {
            if b1 > b0 + 0.05 && !bands.iter().any(|(a, c)| (*a - b0).abs() < 0.05 && (*c - b1).abs() < 0.05) {
                bands.push((b0, b1));
            }
        }
    } }
    let t_geo = t0.elapsed().as_secs_f64() * 1000.0;

    let t1 = Instant::now();
    let mut best: Option<(String, f32)> = None;
    let mut has_strip = false;
    for (b0, b1) in &bands {
        let Some(mut strip) = unwrap_ring_band(&base, &strength, *b0, *b1, geo) else { continue };
        has_strip = true;
        strip_normalize(&mut strip);
        let rgb = strip_to_rgb(&strip);
        let sw = rgb.width() as usize;
        let col: Vec<f64> = (0..sw).map(|x| {
            (0..rgb.height()).map(|y| rgb.get_pixel(x as u32, y).0[0] as f64).sum::<f64>()
        }).collect();
        let mut cuts = vec![0usize];
        for k in 1..4 {
            let target = sw * k / 4;
            let lo = target.saturating_sub(70).max(cuts[k - 1] + 40);
            let hi = (target + 70).min(sw - 40);
            let mut cbest = target; let mut bestv = f64::MAX;
            for x in lo..hi { if col[x] < bestv { bestv = col[x]; cbest = x; } }
            cuts.push(cbest);
        }
        cuts.push(sw);
        let mut crops: Vec<RgbImage> = Vec::new();
        for k in 0..4 {
            let (x0, x1) = (cuts[k], cuts[k + 1]);
            let wcut = x1 - x0;
            if wcut < 8 { continue; }
            crops.push(imageops::crop_imm(&rgb, x0 as u32, 0, wcut as u32, rgb.height()).to_image());
        }
        let texts = rec_batch(rec, dict, &crops, batch)?;
        let mut joined = String::new();
        let (mut sum, mut cnt) = (0f32, 0usize);
        for (t, c) in texts {
            if !t.trim().is_empty() { joined.push_str(&t); joined.push(' '); }
            sum += c; cnt += 1;
        }
        let conf = if cnt > 0 { sum / cnt as f32 } else { 0.0 };
        let score = conf * (joined.chars().count() as f32).sqrt();
        let better = match &best {
            None => true,
            Some((bt, bc)) => score > bc * (bt.chars().count() as f32).sqrt(),
        };
        if better {
            best = Some((joined, conf));
        }
    }
    let text = best.clone().map(|(t, _)| t).unwrap_or_default();
    let t_ocr = t1.elapsed().as_secs_f64() * 1000.0;
    let score = best.as_ref().map(|(t, c)| c * (t.chars().count() as f32).sqrt()).unwrap_or(0.0);
    let _ = has_strip;
    Ok((text.trim().to_string(), best.map(|(_, c)| c).unwrap_or(0.0), score, t_geo, t_ocr, bands.len()))
}

fn process_one(rec: &Model, dict: &[String], batch: usize, path: &Path, file: &str, truth: &str) -> Result<(serde_json::Value, f64, f64)> {
    let img = image::open(path)?.to_rgb8();
    let ch_mode = std::env::var("OCR_CH").unwrap_or_else(|_| "auto".into());
    let primary = if ch_mode == "dark" || ch_mode == "both" { "rg" } else { ch_mode.as_str() };
    let (t1, _c1, _s1, g1, o1, nb1) = run_channel(rec, dict, batch, &img, primary)?;
    let (mut best_text, mut g_ms, mut o_ms, mut nbands) = (t1.clone(), g1, o1, nb1);
    // rg 通道读不出东西（<8 字）时，再试暗度通道——淡章/灰章在暗度上更清楚
    if ch_mode == "auto" || ch_mode == "both" {
        let cjk = t1.chars().filter(|c| ('\u{4e00}'..='\u{9fff}').contains(c)).count();
        if cjk < 4 {
            let (t2, _c2, _s2, g2, o2, nb2) = run_channel(rec, dict, batch, &img, "dark")?;
            g_ms += g2; o_ms += o2; nbands += nb2;
            let cjk2 = t2.chars().filter(|c| ('\u{4e00}'..='\u{9fff}').contains(c)).count();
            if cjk2 > cjk { best_text = t2; }
        }
    }
    let r = recall(truth, &best_text);
    Ok((serde_json::json!({
        "file": file, "truth": truth, "pred": best_text, "recall": (r*1000.0).round()/1000.0,
        "channels": ch_mode, "bands": nbands
    }), g_ms, o_ms))
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let root = PathBuf::from(args.get(1).context("usage: stamp_pipeline <data-dir> <images-dir>")?);
    let imgs_dir = PathBuf::from(args.get(2).context("images dir")?);
    let batch: usize = std::env::var("OCR_BATCH").ok().and_then(|v| v.parse().ok()).unwrap_or(4);
    let threads: usize = std::env::var("OCR_THREADS").ok().and_then(|v| v.parse().ok()).unwrap_or(1);
    let dict: Vec<String> = std::fs::read_to_string(root.join("dict.txt"))?.lines().map(|s| s.to_string()).collect();
    let manifest: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(root.join("detest.json"))?)?;
    let items: Vec<(String, String)> = manifest.as_array().unwrap().iter()
        .map(|it| (it["file"].as_str().unwrap().to_string(), it["truth"].as_str().unwrap().to_string()))
        .collect();

    let t_all = Instant::now();
    let rec_path = root.join(format!("rec_batch{batch}.onnx"));
    let mut results: Vec<(serde_json::Value, f64, f64)> = Vec::new();
    if threads <= 1 {
        let rec = load(&rec_path, [batch, 3, REC_H, REC_W])?;
        for (file, truth) in &items {
            results.push(process_one(&rec, &dict, batch, &imgs_dir.join(file), file, truth)?);
        }
    } else {
        let chunk = (items.len() + threads - 1) / threads;
        let dict_ref = &dict;
        let rec_path_ref = &rec_path;
        let imgs_ref = &imgs_dir;
        let parts: Vec<Vec<(serde_json::Value, f64, f64)>> = std::thread::scope(|sc| {
            let mut handles = Vec::new();
            for chunk_items in items.chunks(chunk.max(1)) {
                handles.push(sc.spawn(move || -> Result<Vec<(serde_json::Value, f64, f64)>> {
                    let rec = load(rec_path_ref, [batch, 3, REC_H, REC_W])?;
                    let mut out = Vec::new();
                    for (file, truth) in chunk_items {
                        out.push(process_one(&rec, dict_ref, batch, &imgs_ref.join(file), file, truth)?);
                    }
                    Ok(out)
                }));
            }
            handles.into_iter().map(|h| h.join().unwrap().unwrap_or_default()).collect()
        });
        for p in parts { results.extend(p); }
    }
    let wall = t_all.elapsed().as_secs_f64() * 1000.0;
    let n = results.len().max(1);
    let t_geo: f64 = results.iter().map(|r| r.1).sum();
    let t_ocr: f64 = results.iter().map(|r| r.2).sum();
    let rows: Vec<&serde_json::Value> = results.iter().map(|r| &r.0).collect();
    for r in &rows {
        println!("{:>2} recall={:.2} warp={} truth={} pred={}", r["file"].as_str().unwrap_or("").len(), r["recall"].as_f64().unwrap_or(0.0),
                 r["warped"].as_bool().unwrap_or(false), r["truth"].as_str().unwrap_or(""), r["pred"].as_str().unwrap_or("").chars().take(36).collect::<String>());
    }
    let avg: f64 = rows.iter().map(|r| r["recall"].as_f64().unwrap_or(0.0)).sum::<f64>() / n as f64;
    std::fs::write(root.join("baseline_pipeline.json"), serde_json::to_string_pretty(&serde_json::json!({
        "engine": "tract-rust(geom+unwrap+split+rec)", "rows": rows, "images": n, "batch": batch, "threads": threads,
        "avg_ms_geom": (t_geo / n as f64 * 10.0).round() / 10.0,
        "avg_ms_ocr": (t_ocr / n as f64 * 10.0).round() / 10.0,
        "wall_ms_total": (wall * 10.0).round() / 10.0,
        "wall_ms_per_image": (wall / n as f64 * 10.0).round() / 10.0,
        "avg_recall": (avg * 1000.0).round() / 1000.0
    }))?)?;
    println!("\n平均字覆盖 {:.3}  几何 {:.1}ms/枚  识别 {:.1}ms/枚  | batch {} threads {} 墙钟 {:.0}ms（{:.0}ms/枚）",
             avg, t_geo / n as f64, t_ocr / n as f64, batch, threads, wall, wall / n as f64);
    Ok(())
}
