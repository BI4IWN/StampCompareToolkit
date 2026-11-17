use anyhow::{Context, Result};
use image::imageops;
use std::{env, fs, path::PathBuf, time::Instant};
use tract_onnx::prelude::*;

const REC_H: usize = 48;
const IMG_W: usize = 320;

fn main() -> Result<()> {
    let args: Vec<String> = env::args().collect();
    let root = PathBuf::from(args.get(1).context("usage: ocr-tract <data-dir>")?);
    let model_path = root.join("rec_static.onnx");
    let dict: Vec<String> = fs::read_to_string(root.join("dict.txt"))?.lines().map(|s| s.to_string()).collect();

    let model = tract_onnx::onnx()
        .model_for_path(&model_path)?
        .with_input_fact(0, f32::fact([1, 3, REC_H, IMG_W]).into())?
        .into_optimized()?
        .into_runnable()?;

    let manifest: serde_json::Value = serde_json::from_str(&fs::read_to_string(root.join("manifest.json"))?)?;
    let mut rows = Vec::new();
    let mut total = 0f64;
    let mut n = 0usize;

    for s in manifest.as_array().unwrap() {
        let i = s["i"].as_u64().unwrap_or(0);
        let truth = s["truth"].as_str().unwrap_or("");
        let mut pred = String::new();
        for k in 0..4 {
            let p = root.join("strips").join(format!("{i:02}_{k}.png"));
            if !p.exists() { continue; }
            let img = image::open(&p)?.to_rgb8();
            let (w, h) = (img.width() as f32, img.height() as f32);
            let resized_w = ((REC_H as f32 * (w / h)).ceil() as usize).min(IMG_W);
            let rs = imageops::resize(&img, resized_w as u32, REC_H as u32, imageops::FilterType::Triangle);
            let mut data = vec![0f32; 3 * REC_H * IMG_W];
            for y in 0..REC_H {
                for x in 0..resized_w {
                    let px = rs.get_pixel(x as u32, y as u32).0;
                    for c in 0..3 {
                        data[c * REC_H * IMG_W + y * IMG_W + x] = (px[c] as f32 / 255.0 - 0.5) / 0.5;
                    }
                }
            }
            let input = tract_ndarray::Array4::from_shape_vec((1, 3, REC_H, IMG_W), data)?;
            let t0 = Instant::now();
            let out = model.run(tvec!(input.into_tensor().into()))?;
            total += t0.elapsed().as_secs_f64() * 1000.0;
            n += 1;
            let o = out[0].to_array_view::<f32>()?;
            let shape = o.shape().to_vec();
            let (steps, classes) = (shape[1], shape[2]);
            let (mut text, mut prev) = (String::new(), usize::MAX);
            for t in 0..steps {
                let mut best = f32::MIN; let mut bi = 0usize;
                for c in 0..classes {
                    let v = o[[0, t, c]];
                    if v > best { best = v; bi = c; }
                }
                if bi != 0 && bi != prev {
                    let idx = bi - 1;
                    if idx < dict.len() { text.push_str(&dict[idx]); } else { text.push(' '); }
                }
                prev = bi;
            }
            pred.push_str(&text);
        }
        let mut tt = std::collections::HashMap::new();
        for ch in truth.chars() { *tt.entry(ch).or_insert(0i32) += 1; }
        let mut hit = 0;
        for ch in pred.chars() { if let Some(c) = tt.get_mut(&ch) { if *c > 0 { *c -= 1; hit += 1 } } }
        let recall = hit as f64 / truth.chars().count().max(1) as f64;
        rows.push(serde_json::json!({"i": i, "truth": truth, "pred": pred, "recall": (recall*1000.0).round()/1000.0}));
    }
    let avg: f64 = rows.iter().map(|r| r["recall"].as_f64().unwrap_or(0.0)).sum::<f64>() / rows.len() as f64;
    fs::write(root.join("baseline_tract.json"), serde_json::to_string_pretty(&serde_json::json!({
        "engine": "tract-rust(PP-OCRv4 rec)", "rows": rows, "strips": n, "avg_ms_per_strip": (total/n as f64*10.0).round()/10.0
    }))?)?;
    println!("tract  平均字覆盖 {:.3}  平均每条 {:.1}ms  共 {} 条", avg, total / n as f64, n);
    for r in rows.iter().take(6) {
        println!("  {:02} {:.2}  {}  ->  {}", r["i"], r["recall"].as_f64().unwrap_or(0.0), r["truth"].as_str().unwrap_or(""), &r["pred"].as_str().unwrap_or("").chars().take(36).collect::<String>());
    }
    Ok(())
}
