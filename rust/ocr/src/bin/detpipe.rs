// DB 检测器（PP-OCRv4 det）+ 识别器（PP-OCRv4 rec）全 Rust 端到端
// 用法: detpipe <data-dir> <images-dir>
use anyhow::{Context, Result};
use image::{imageops, RgbImage};
use std::path::{Path, PathBuf};
use std::time::Instant;
use tract_onnx::prelude::*;


const DET_LIMIT: f32 = 736.0;     // PaddleOCR limit_side_len，limit_type=min
const THRESH: f32 = 0.3;
const BOX_THRESH: f32 = 0.5;
const UNCLIP: f32 = 1.6;
const MIN_SIZE: f32 = 3.0;
const REC_H: usize = 48;
const REC_W: usize = 320;

type Model = SimplePlan<TypedFact, Box<dyn TypedOp>, TypedModel>;

fn load(path: &Path, shape: [usize; 4]) -> Result<Model> {
    Ok(tract_onnx::onnx()
        .model_for_path(path)?
        .with_input_fact(0, f32::fact(shape).into())?
        .into_optimized()?
        .into_runnable()?)
}

struct DetInput {
    data: Vec<f32>,
    content_w: usize,
    content_h: usize,
}

fn det_side() -> usize {
    std::env::var("OCR_DET_SIDE").ok().and_then(|v| v.parse().ok()).unwrap_or(640)
}

fn preprocess_det(img: &RgbImage, side: usize) -> DetInput {
    let (w, h) = (img.width() as f32, img.height() as f32);
    let mut scale = DET_LIMIT / w.min(h);
    if w.max(h) * scale > side as f32 {
        scale = side as f32 / w.max(h);
    }
    // 对齐到 32 的倍数（向下取整，至少 32）
    let cw = (((w * scale) as usize) / 32 * 32).max(32);
    let ch = (((h * scale) as usize) / 32 * 32).max(32);
    let rs = imageops::resize(img, cw as u32, ch as u32, imageops::FilterType::Triangle);
    let mut data = vec![0f32; 3 * side * side];
    for y in 0..ch {
        for x in 0..cw {
            let px = rs.get_pixel(x as u32, y as u32).0;
            for c in 0..3 {
                data[c * side * side + y * side + x] = (px[c] as f32 / 255.0 - 0.5) / 0.5;
            }
        }
    }
    DetInput { data, content_w: cw, content_h: ch }
}

#[derive(Clone, Copy, Debug)]
struct Point { x: f32, y: f32 }

#[derive(Clone, Debug)]
struct Box4 { pts: [Point; 4], score: f32 }

/// 连通域 + PCA 定向框
fn boxes_from_prob(prob: &[f32], w: usize, h: usize) -> Vec<Box4> {
    let dbg = std::env::var("OCRDBG").is_ok();
    let (mut ncomp, mut maxscore) = (0usize, 0f32);
    let mut seen = vec![false; w * h];
    let mut out = Vec::new();
    let mut stack: Vec<usize> = Vec::new();
    for start in 0..w * h {
        if seen[start] || prob[start] < THRESH { continue; }
        let mut pts: Vec<(f32, f32)> = Vec::new();
        let mut score_sum = 0f64;
        stack.clear();
        stack.push(start);
        seen[start] = true;
        while let Some(idx) = stack.pop() {
            let (x, y) = (idx % w, idx / w);
            pts.push((x as f32, y as f32));
            score_sum += prob[idx] as f64;
            for dy in -1i32..=1 {
                for dx in -1i32..=1 {
                    if dx == 0 && dy == 0 { continue; }
                    let nx = x as i32 + dx;
                    let ny = y as i32 + dy;
                    if nx < 0 || ny < 0 || nx >= w as i32 || ny >= h as i32 { continue; }
                    let nidx = ny as usize * w + nx as usize;
                    if !seen[nidx] && prob[nidx] >= THRESH {
                        seen[nidx] = true;
                        stack.push(nidx);
                    }
                }
            }
        }
        if pts.len() < 12 { continue; }
        ncomp += 1;
        let n = pts.len() as f32;
        let (mut mx, mut my) = (0f32, 0f32);
        for p in &pts { mx += p.0; my += p.1; }
        mx /= n; my /= n;
        let (mut cxx, mut cyy, mut cxy) = (0f32, 0f32, 0f32);
        for p in &pts {
            let dx = p.0 - mx; let dy = p.1 - my;
            cxx += dx * dx; cyy += dy * dy; cxy += dx * dy;
        }
        cxx /= n; cyy /= n; cxy /= n;
        // 主轴角
        let theta = 0.5 * (2.0 * cxy).atan2(cxx - cyy);
        let (ct, st) = (theta.cos(), theta.sin());
        let (mut u0, mut u1, mut v0, mut v1) = (f32::MAX, f32::MIN, f32::MAX, f32::MIN);
        for p in &pts {
            let dx = p.0 - mx; let dy = p.1 - my;
            let u = dx * ct + dy * st;
            let v = -dx * st + dy * ct;
            u0 = u0.min(u); u1 = u1.max(u); v0 = v0.min(v); v1 = v1.max(v);
        }
        let bw = u1 - u0; let bh = v1 - v0;
        if bw < MIN_SIZE || bh < MIN_SIZE { continue; }
        // unclip：按面积/周长等比外扩
        // 分数按「未外扩」的紧框算（与 PaddleOCR 的 box_score 一致），外扩只影响最终框
        let (mut sum, mut cnt) = (0f64, 0usize);
        for iy in 0..6 {
            for ix in 0..12 {
                let u = u0 + bw * (ix as f32 + 0.5) / 12.0;
                let v = v0 + bh * (iy as f32 + 0.5) / 6.0;
                let px = mx + u * ct - v * st;
                let py = my + u * st + v * ct;
                let xi = px.round() as i32; let yi = py.round() as i32;
                if xi < 0 || yi < 0 || xi >= w as i32 || yi >= h as i32 { continue; }
                sum += prob[yi as usize * w + xi as usize] as f64;
                cnt += 1;
            }
        }
        let score = if cnt > 0 { (sum / cnt as f64) as f32 } else { 0.0 };
        if score > maxscore { maxscore = score; }
        if score < BOX_THRESH { continue; }
        // unclip：按面积/周长等比外扩
        let d = bw * bh * UNCLIP / (2.0 * (bw + bh));
        let (nu0, nu1, nv0, nv1) = (u0 - d, u1 + d, v0 - d, v1 + d);
        // 四个角（长轴方向 = (ct, st)）
        let corner = |u: f32, v: f32| Point { x: mx + u * ct - v * st, y: my + u * st + v * ct };
        out.push(Box4 { pts: [corner(nu0, nv0), corner(nu1, nv0), corner(nu1, nv1), corner(nu0, nv1)], score });
    }
    if dbg { eprintln!("  comps {} max_score {:.3} kept {}", ncomp, maxscore, out.len()); }
    out.sort_by(|a, b| {
        let ay = a.pts.iter().map(|p| p.y).sum::<f32>() / 4.0;
        let by = b.pts.iter().map(|p| p.y).sum::<f32>() / 4.0;
        let ax = a.pts.iter().map(|p| p.x).sum::<f32>() / 4.0;
        let bx = b.pts.iter().map(|p| p.x).sum::<f32>() / 4.0;
        (ay as i32 / 24).cmp(&(by as i32 / 24)).then(ax.partial_cmp(&bx).unwrap())
    });
    out
}

/// 透视裁剪（逆映射 + 双线性）
fn crop_obb(img: &RgbImage, b: &Box4) -> RgbImage {
    let (p0, p1, p3) = (b.pts[0], b.pts[1], b.pts[3]);
    let wlen = ((p1.x - p0.x).powi(2) + (p1.y - p0.y).powi(2)).sqrt().max(1.0);
    let hlen = ((p3.x - p0.x).powi(2) + (p3.y - p0.y).powi(2)).sqrt().max(1.0);
    let (ow, oh) = (wlen.ceil() as u32, hlen.ceil() as u32);
    let mut out = RgbImage::new(ow.max(1), oh.max(1));
    for y in 0..oh {
        for x in 0..ow {
            let u = x as f32 / ow as f32;
            let v = y as f32 / oh as f32;
            let sx = p0.x + (p1.x - p0.x) * u + (p3.x - p0.x) * v;
            let sy = p0.y + (p1.y - p0.y) * u + (p3.y - p0.y) * v;
            if sx < 0.0 || sy < 0.0 || sx >= img.width() as f32 - 1.0 || sy >= img.height() as f32 - 1.0 { continue; }
            let (x0, y0) = (sx as u32, sy as u32);
            let (fx, fy) = (sx - x0 as f32, sy - y0 as f32);
            let p00 = img.get_pixel(x0, y0).0;
            let p10 = img.get_pixel(x0 + 1, y0).0;
            let p01 = img.get_pixel(x0, y0 + 1).0;
            let p11 = img.get_pixel(x0 + 1, y0 + 1).0;
            let mut px = [0u8; 3];
            for c in 0..3 {
                let top = p00[c] as f32 * (1.0 - fx) + p10[c] as f32 * fx;
                let bot = p01[c] as f32 * (1.0 - fx) + p11[c] as f32 * fx;
                px[c] = (top * (1.0 - fy) + bot * fy).round().clamp(0.0, 255.0) as u8;
            }
            out.put_pixel(x, y, image::Rgb(px));
        }
    }
    out
}

/// 文本行方向分类：输出 2 类（0=正，1=倒）；返回是否需 180° 旋转
fn need_rotate(cls: &Model, img: &RgbImage) -> Result<bool> {
    let rs = imageops::resize(img, 192, 48, imageops::FilterType::Triangle);
    let mut data = vec![0f32; 3 * 48 * 192];
    for y in 0..48 {
        for x in 0..192 {
            let px = rs.get_pixel(x as u32, y as u32).0;
            for c in 0..3 {
                data[c * 48 * 192 + y * 192 + x] = (px[c] as f32 / 255.0 - 0.5) / 0.5;
            }
        }
    }
    let input = tract_ndarray::Array4::from_shape_vec((1, 3, 48, 192), data)?;
    let out = cls.run(tvec!(input.into_tensor().into()))?;
    let o = out[0].to_array_view::<f32>()?;
    let (a, b) = (o[[0, 0]], o[[0, 1]]);
    Ok(b > a)
}

fn rec_text(model: &Model, dict: &[String], img: &RgbImage) -> Result<String> {
    let (w, h) = (img.width() as f32, img.height() as f32);
    if w < 2.0 || h < 2.0 { return Ok(String::new()); }
    let rw = ((REC_H as f32 * (w / h)).ceil() as usize).clamp(4, REC_W);
    let rs = imageops::resize(img, rw as u32, REC_H as u32, imageops::FilterType::Triangle);
    let mut data = vec![0f32; 3 * REC_H * REC_W];
    for y in 0..REC_H {
        for x in 0..rw {
            let px = rs.get_pixel(x as u32, y as u32).0;
            for c in 0..3 {
                data[c * REC_H * REC_W + y * REC_W + x] = (px[c] as f32 / 255.0 - 0.5) / 0.5;
            }
        }
    }
    let input = tract_ndarray::Array4::from_shape_vec((1, 3, REC_H, REC_W), data)?;
    let out = model.run(tvec!(input.into_tensor().into()))?;
    let o = out[0].to_array_view::<f32>()?;
    let (steps, classes) = (o.shape()[1], o.shape()[2]);
    let (mut text, mut prev) = (String::new(), usize::MAX);
    for t in 0..steps {
        let mut best = f32::MIN; let mut bi = 0usize;
        for c in 0..classes {
            let v = o[[0, t, c]];
            if v > best { best = v; bi = c; }
        }
        if bi != 0 && bi != prev {
            let idx = bi - 1;
            if idx < dict.len() { text.push_str(&dict[idx]); }
        }
        prev = bi;
    }
    Ok(text)
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let root = PathBuf::from(args.get(1).context("usage: detpipe <data-dir> <images-dir>")?);
    let imgs_dir = PathBuf::from(args.get(2).context("images dir")?);
    let side = det_side();
    let det = load(&root.join(format!("det_static{side}.onnx")), [1, 3, side, side])?;
    let rec = load(&root.join("rec_static.onnx"), [1, 3, REC_H, REC_W])?;
    let cls = load(&root.join("cls_static.onnx"), [1, 3, 48, 192])?;
    let dict: Vec<String> = std::fs::read_to_string(root.join("dict.txt"))?
        .lines().map(|s| s.to_string()).collect();

    let manifest: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(root.join("detest.json"))?)?;
    let mut rows = Vec::new();
    let (mut t_det, mut t_rec, mut n_img) = (0f64, 0f64, 0usize);

    for it in manifest.as_array().unwrap() {
        let file = it["file"].as_str().unwrap();
        let truth = it["truth"].as_str().unwrap();
        let img = image::open(imgs_dir.join(file))?.to_rgb8();
        let di = preprocess_det(&img, side);
        let input = tract_ndarray::Array4::from_shape_vec((1, 3, side, side), di.data.clone())?;
        let t0 = Instant::now();
        let out = det.run(tvec!(input.into_tensor().into()))?;
        t_det += t0.elapsed().as_secs_f64() * 1000.0;
        let o = out[0].to_array_view::<f32>()?;
        let (ph, pw) = (o.shape()[2], o.shape()[3]);
        let mut prob = vec![0f32; pw * ph];
        for y in 0..ph { for x in 0..pw { prob[y * pw + x] = o[[0, 0, y, x]]; } }
        if std::env::var("OCRDBG").is_ok() && rows.is_empty() {
            let mx = prob.iter().cloned().fold(f32::MIN, f32::max);
            let mn = prob.iter().cloned().fold(f32::MAX, f32::min);
            let mean = prob.iter().sum::<f32>() / prob.len() as f32;
            eprintln!("prob shape {:?} min {:.3} max {:.3} mean {:.4} content {}x{}", o.shape(), mn, mx, mean, di.content_w, di.content_h);
        }
        let boxes = boxes_from_prob(&prob, pw, ph);
        // 概率图坐标 → 原图坐标
        let mut texts = Vec::new();
        let t1 = Instant::now();
        for b in &boxes {
            let (sx, sy) = (img.width() as f32 / di.content_w as f32, img.height() as f32 / di.content_h as f32);
            let scaled = Box4 {
                pts: b.pts.map(|p| Point { x: p.x * sx, y: p.y * sy }),
                score: b.score,
            };
            let mut crop = crop_obb(&img, &scaled);
            if crop.width() < 4 || crop.height() < 4 { continue; }
            if need_rotate(&cls, &crop).unwrap_or(false) {
                crop = imageops::rotate180(&crop);
            }
            let s = rec_text(&rec, &dict, &crop)?;
            if !s.trim().is_empty() { texts.push(s); }
        }
        t_rec += t1.elapsed().as_secs_f64() * 1000.0;
        n_img += 1;
        let joined = texts.join(" ");
        let mut tt = std::collections::HashMap::new();
        for ch in truth.chars() { *tt.entry(ch).or_insert(0i32) += 1; }
        let mut hit = 0;
        for ch in joined.chars() { if let Some(c) = tt.get_mut(&ch) { if *c > 0 { *c -= 1; hit += 1 } } }
        let recall = hit as f64 / truth.chars().count().max(1) as f64;
        println!("{:>2} boxes={:<3} recall={:.2} truth={} pred={}", rows.len(), boxes.len(), recall, truth, joined.chars().take(40).collect::<String>());
        rows.push(serde_json::json!({
            "file": file, "truth": truth, "pred": joined, "recall": (recall*1000.0).round()/1000.0,
            "boxes": boxes.len(), "det_w": di.content_w, "det_h": di.content_h
        }));
    }
    let avg: f64 = rows.iter().map(|r| r["recall"].as_f64().unwrap_or(0.0)).sum::<f64>() / rows.len().max(1) as f64;
    std::fs::write(root.join("baseline_detpipe.json"), serde_json::to_string_pretty(&serde_json::json!({
        "engine": "tract-rust(det+rec)", "rows": rows, "images": n_img,
        "avg_ms_det": (t_det / n_img as f64 * 10.0).round() / 10.0,
        "avg_ms_rec": (t_rec / n_img as f64 * 10.0).round() / 10.0,
        "avg_recall": (avg * 1000.0).round() / 1000.0
    }))?)?;
    println!("\n平均字覆盖 {:.3}  det {:.1}ms/图  rec {:.1}ms/图  ({} 张)", avg, t_det / n_img as f64, t_rec / n_img as f64, n_img);
    Ok(())
}
