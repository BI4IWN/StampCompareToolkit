mod fit;
mod dewarp;
mod align;
mod gates;
mod synth;

use image::RgbaImage;

use std::path::PathBuf;

fn load(p: &str) -> RgbaImage {
    image::open(p).expect("open image").to_rgba8()
}

fn alpha_of(img: &RgbaImage) -> Vec<u8> { img.pixels().map(|p| p.0[3]).collect() }

fn cmd_fit(args: &[String]) {
    let mut outs = Vec::new();
    for f in args {
        let img = load(f);
        let (w, h) = (img.width() as usize, img.height() as usize);
        let al = alpha_of(&img);
        if let Some(e) = fit::fit_ellipse(w, h, &al) {
            let g = gates::check(w, h, &al, &e);
            outs.push(serde_json::json!({
                "file": f, "ratio": e.ratio, "a": e.a, "b": e.b, "theta_deg": e.theta_deg,
                "coverage": e.coverage, "inlier_ratio": e.inlier_ratio, "points": e.points,
                "cx": e.cx, "cy": e.cy, "ink_fill": g.ink_fill, "gate_ok": g.ok, "gate_reasons": g.reasons,
            }));
        }
    }
    println!("{}", serde_json::to_string_pretty(&outs).unwrap());
}

fn cmd_dewarp(args: &[String]) {
    let mut outs = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let f = &args[i];
        let out_path = if i + 2 < args.len() && args[i + 1] == "--out" { Some(PathBuf::from(&args[i + 2])) } else { None };
        let img = load(f);
        let t0 = std::time::Instant::now();
        if let Some(r) = dewarp::dewarp_iterative(&img) {
            let ms = t0.elapsed().as_secs_f64() * 1000.0;
            if let Some(p) = &out_path { r.image.save(p).ok(); }
            outs.push(serde_json::json!({
                "file": f,
                "before_ratio": r.before.ratio,
                "after_ratio": r.after.as_ref().map(|e| e.ratio).unwrap_or(f64::NAN),
                "rounds": r.rounds,
                "radius": r.radius,
                "ms": ms,
                "out": out_path.as_ref().map(|p| p.to_string_lossy().to_string()),
            }));
        }
        i += if out_path.is_some() { 3 } else { 1 };
    }
    println!("{}", serde_json::to_string_pretty(&outs).unwrap());
}

fn cmd_align(args: &[String]) {
    if args.len() < 2 { eprintln!("align <a.png> <b.png>"); return; }
    let a = load(&args[0]);
    let b = load(&args[1]);
    let (w, h) = (a.width() as usize, a.height() as usize);
    let (aa, ab) = (alpha_of(&a), alpha_of(&b));
    match align::align_from_images(&aa, w, h, &ab) {
        Some((r, ea, eb)) => println!("{}", serde_json::to_string_pretty(&serde_json::json!({
            "d_center_px": r.d_center_px,
            "radius_ratio": r.radius_ratio,
            "rotate_deg": r.rotate_deg,
            "a_ratio": ea.ratio, "b_ratio": eb.ratio,
            "a_theta": ea.theta_deg, "b_theta": eb.theta_deg,
        })).unwrap()),
        None => println!("{}", serde_json::json!({"error": "fit failed"})),
    }
}

fn cmd_synth(dir: &str) {
    std::fs::create_dir_all(dir).ok();
    let mut meta = Vec::new();
    for &ratio in &[0.76f64, 0.82, 0.85, 0.90] {
        for &rot in &[0.0f64, 15.0, 22.0, 30.0] {
            let name = format!("{}/synth_r{:.2}_t{:02.0}.png", dir, ratio, rot);
            let img = synth::make_stamp(600, ratio, rot, [192, 57, 43]);
            img.save(&name).ok();
            meta.push(serde_json::json!({"file": name, "truth_ratio": ratio, "truth_rot": rot}));
        }
    }
    println!("{}", serde_json::to_string_pretty(&meta).unwrap());
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() { eprintln!("usage: stamp-core <fit|dewarp|align|synth> ..."); return; }
    match args[0].as_str() {
        "fit" => cmd_fit(&args[1..]),
        "dewarp" => cmd_dewarp(&args[1..]),
        "align" => cmd_align(&args[1..]),
        "synth" => cmd_synth(args.get(1).map(|s| s.as_str()).unwrap_or("samples/synth")),
        other => eprintln!("unknown command: {}", other),
    }
}
