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
    let mut out = vec![0u8; (img.width() * img.height()) as usize];
    for (i, px) in img.pixels().enumerate() {
        let (r, g, b) = (px.0[0] as i32, px.0[1] as i32, px.0[2] as i32);
        let v = if use_red { (r - g).max(0).min(255) } else { (255 - r.min(g).min(b)).max(0) };
        out[i] = v as u8;
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
fn unwrap_ring(img: &RgbaImage, strength: &[u8]) -> Option<image::GrayImage> {
    let (w, h) = (img.width() as usize, img.height() as usize);
    let alpha = alpha_of(img);
    let (cx, cy) = stamp_core::fit::mask_centroid(w, h, &alpha);
    let mut d: Vec<f64> = Vec::new();
    for y in 0..h { for x in 0..w {
        if alpha[y * w + x] > 40 {
            d.push(((x as f64 - cx).powi(2) + (y as f64 - cy).powi(2)).sqrt());
        }
    }}
    if d.len() < 100 { return None; }
    d.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let r = d[(d.len() as f64 * 0.992) as usize];
    if r < 20.0 { return None; }
    let mut strip = image::GrayImage::new(STRIP_W as u32, STRIP_H as u32);
    for i in 0..STRIP_W {
        let ang = std::f64::consts::TAU * (i as f64) / (STRIP_W as f64);
        for j in 0..STRIP_H {
            let rad = r * (BAND_R0 + (BAND_R1 - BAND_R0) * (j as f64) / ((STRIP_H - 1) as f64));
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

fn strip_to_rgb(s: &image::GrayImage) -> RgbImage {
    let mut out = RgbImage::new(s.width(), s.height());
    for (x, y, p) in s.enumerate_pixels() {
        out.put_pixel(x, y, image::Rgb([p.0[0], p.0[0], p.0[0]]));
    }
    out
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

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let root = PathBuf::from(args.get(1).context("usage: stamp_pipeline <data-dir> <images-dir>")?);
    let imgs_dir = PathBuf::from(args.get(2).context("images dir")?);
    let rec = load(&root.join("rec_static.onnx"), [1, 3, REC_H, REC_W])?;
    let dict: Vec<String> = std::fs::read_to_string(root.join("dict.txt"))?.lines().map(|s| s.to_string()).collect();
    let manifest: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(root.join("detest.json"))?)?;

    let mut rows = Vec::new();
    let (mut t_geo, mut t_ocr, mut n) = (0f64, 0f64, 0usize);
    let (mut n_warp, mut n_ok) = (0usize, 0usize);

    for it in manifest.as_array().unwrap() {
        let file = it["file"].as_str().unwrap();
        let truth = it["truth"].as_str().unwrap();
        let img = image::open(imgs_dir.join(file))?.to_rgb8();
        let t0 = Instant::now();
        let masked = ink_mask(&img);
        let (w, h) = (masked.width() as usize, masked.height() as usize);
        let alpha = alpha_of(&masked);
        let mut base = masked;
        let mut warp_ratio = 1.0f64;
        if let Some(el) = stamp_core::fit::fit_ellipse(w, h, &alpha) {
            let rep = stamp_core::gates::check(w, h, &alpha, &el);
            if rep.ok {
                if let Some(dw) = stamp_core::dewarp_iterative(&base) {
                    warp_ratio = dw.before.ratio / dw.after.map(|e| e.ratio).unwrap_or(dw.before.ratio).max(1e-6);
                    base = dw.image;
                    n_warp += 1;
                }
            }
        }
        let base_rgb: RgbImage = {
            let mut o = RgbImage::new(base.width(), base.height());
            for (x, y, p) in base.enumerate_pixels() { o.put_pixel(x, y, image::Rgb([p.0[0], p.0[1], p.0[2]])); }
            o
        };
        let (strength, _) = ink_strength(&base_rgb);
        let strip = unwrap_ring(&base, &strength);
        let t_geo_ms = t0.elapsed().as_secs_f64() * 1000.0;
        t_geo += t_geo_ms;

        let t1 = Instant::now();
        let mut text = String::new();
        if let Some(s) = &strip {
            let rgb = strip_to_rgb(s);
            // 先按四等分取切点，再把切点吸附到「字符间隙」（列投影最小处），避免切断字
            let sw = rgb.width() as usize;
            let col: Vec<f64> = (0..sw).map(|x| {
                (0..rgb.height()).map(|y| rgb.get_pixel(x as u32, y).0[0] as f64).sum::<f64>()
            }).collect();
            let mut cuts = vec![0usize];
            for k in 1..4 {
                let target = sw * k / 4;
                let lo = target.saturating_sub(70).max(cuts[k - 1] + 40);
                let hi = (target + 70).min(sw - 40);
                let mut best = target; let mut bestv = f64::MAX;
                for x in lo..hi { if col[x] < bestv { bestv = col[x]; best = x; } }
                cuts.push(best);
            }
            cuts.push(sw);
            for k in 0..4 {
                let (x0, x1) = (cuts[k], cuts[k + 1]);
                let wcut = x1 - x0;
                if wcut < 8 { continue; }
                let crop = imageops::crop_imm(&rgb, x0 as u32, 0, wcut as u32, rgb.height()).to_image();
                text.push_str(&rec_text(&rec, &dict, &crop)?);
                text.push(' ');
            }
            if !text.trim().is_empty() { n_ok += 1; }
        }
        t_ocr += t1.elapsed().as_secs_f64() * 1000.0;
        n += 1;
        let r = recall(truth, &text);
        println!("{:>2} recall={:.2} warp={:.3} truth={} pred={}", rows.len(), r, warp_ratio, truth, text.chars().take(40).collect::<String>());
        rows.push(serde_json::json!({
            "file": file, "truth": truth, "pred": text.trim(), "recall": (r*1000.0).round()/1000.0,
            "warped": warp_ratio != 1.0, "has_strip": strip.is_some()
        }));
    }
    let avg: f64 = rows.iter().map(|r| r["recall"].as_f64().unwrap_or(0.0)).sum::<f64>() / rows.len().max(1) as f64;
    std::fs::write(root.join("baseline_pipeline.json"), serde_json::to_string_pretty(&serde_json::json!({
        "engine": "tract-rust(geom+unwrap+split+rec)", "rows": rows, "images": n,
        "avg_ms_geom": (t_geo / n as f64 * 10.0).round() / 10.0,
        "avg_ms_ocr": (t_ocr / n as f64 * 10.0).round() / 10.0,
        "avg_recall": (avg * 1000.0).round() / 1000.0,
        "dewarped": n_warp, "strips_ok": n_ok
    }))?)?;
    println!("\n平均字覆盖 {:.3}  几何 {:.1}ms  识别 {:.1}ms  共 {} 张（校正 {} 张）", avg, t_geo / n as f64, t_ocr / n as f64, n, n_warp);
    Ok(())
}
