// 椭圆 → 正圆：逆映射 + 双线性采样；迭代直到 ratio>=0.988（最多 3 轮）
use image::RgbaImage;
use crate::fit::{fit_ellipse, Ellipse};
use crate::gates;

pub struct DewarpResult {
    pub image: RgbaImage,
    pub before: Ellipse,
    pub after: Option<Ellipse>,
    pub rounds: u32,
    pub radius: f64,
}

fn sample_bilinear(img: &RgbaImage, x: f64, y: f64) -> [u8; 4] {
    let (w, h) = (img.width() as i64, img.height() as i64);
    if x < 0.0 || y < 0.0 || x >= (w - 1) as f64 || y >= (h - 1) as f64 {
        return [0, 0, 0, 0];
    }
    let (x0, y0) = (x.floor() as i64, y.floor() as i64);
    let (fx, fy) = (x - x0 as f64, y - y0 as f64);
    let px = |xx: i64, yy: i64| -> [u8; 4] {
        let p = img.get_pixel(xx as u32, yy as u32).0;
        [p[0], p[1], p[2], p[3]]
    };
    let (p00, p10, p01, p11) = (px(x0, y0), px(x0 + 1, y0), px(x0, y0 + 1), px(x0 + 1, y0 + 1));
    let mut out = [0u8; 4];
    for c in 0..4 {
        let top = p00[c] as f64 * (1.0 - fx) + p10[c] as f64 * fx;
        let bot = p01[c] as f64 * (1.0 - fx) + p11[c] as f64 * fx;
        out[c] = (top * (1.0 - fy) + bot * fy).round().clamp(0.0, 255.0) as u8;
    }
    out
}

/// 把椭圆拉成正圆并居中；半径归一化到 0.45*min(W,H)
pub fn dewarp_once(img: &RgbaImage, e: &Ellipse) -> RgbaImage {
    let (w, h) = (img.width(), img.height());
    let mut out = RgbaImage::new(w, h);
    let r_target = 0.45 * (w.min(h) as f64);
    let u = r_target / e.a;                 // 统一缩放（同时统一两章尺度）
    // 纵向补偿用"偏差校正后的真实轴比"（e.ratio），不用原始矩估计 a/b（会被射线取样拉向 1）
    let rho = e.ratio.clamp(0.30, 1.0);
    let ky = u / rho;
    let kx = u;
    let (ct, st) = (e.theta_deg.to_radians().cos(), e.theta_deg.to_radians().sin());
    let (ocx, ocy) = (w as f64 / 2.0, h as f64 / 2.0);
    for y in 0..h {
        for x in 0..w {
            // 目标 → 源：先反向旋转，再按 1/k 缩放，最后加回中心
            let dx = x as f64 - ocx;
            let dy = y as f64 - ocy;
            let rx = dx * ct + dy * st;
            let ry = -dx * st + dy * ct;
            // 反变换：先除缩放（kx 沿长轴、ky 沿短轴），再转回章坐标
            let ux = rx / kx;
            let vy = ry / ky;
            let sx = e.cx + ux * ct - vy * st;
            let sy = e.cy + ux * st + vy * ct;
            let p = sample_bilinear(img, sx, sy);
            out.put_pixel(x, y, image::Rgba(p));
        }
    }
    out
}

pub fn dewarp_iterative(img: &RgbaImage) -> Option<DewarpResult> {
    let (w, h) = (img.width() as usize, img.height() as usize);
    let alpha: Vec<u8> = img.pixels().map(|p| p.0[3]).collect();
    let first = fit_ellipse(w, h, &alpha)?;
    // 已经够圆：不处理（前端同款 no-op，不算失败）
    if first.ratio >= 0.995 {
        return Some(DewarpResult { image: img.clone(), before: first.clone(), after: Some(first), rounds: 0, radius: 0.0 });
    }
    // 闸门：不像"一圈章边"的，不做校正（与前端一致）
    let g = gates::check(w, h, &alpha, &first);
    if !g.ok { return None; }
    let max_rounds = if first.ratio > 0.995 { 1 } else { 3 };
    // 只接受"确实更圆"的轮次：变差就回退到上一轮（对齐 JS 版的收益校验）
    let mut cur = img.clone();
    let mut ell = first.clone();
    let mut rounds = 0;
    let mut best_img = img.clone();
    let mut best_ratio = first.ratio;
    let mut best_fit: Option<Ellipse> = None;
    let mut last_fit: Option<Ellipse> = None;
    for _ in 0..max_rounds {
        rounds += 1;
        let cand = dewarp_once(&cur, &ell);
        let a2: Vec<u8> = cand.pixels().map(|p| p.0[3]).collect();
        match fit_ellipse(w, h, &a2) {
            Some(f2) => {
                let r = f2.ratio;
                last_fit = Some(f2.clone());
                if r > best_ratio + 1e-3 {
                    best_ratio = r;
                    best_img = cand.clone();
                    best_fit = Some(f2.clone());
                    ell = f2;
                    cur = cand;
                } else {
                    break;   // 这一轮没有变得更圆，回退
                }
                if r >= 0.988 { break; }
            }
            None => break,
        }
    }
    let cur = best_img;
    let radius = 0.45 * (w.min(h) as f64);
    Some(DewarpResult { image: cur, before: first, after: best_fit.or(last_fit), rounds, radius })
}
