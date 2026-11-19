// 去畸变的可信度闸门（与 stamp-compare.html 的 dewarpExtracted 保持同参数）
use crate::fit::Ellipse;

pub struct GateReport {
    pub ok: bool,
    pub reasons: Vec<String>,
    pub ink_fill: f64,
}

/// 墨迹填充率：alpha>40 的像素数 / 椭圆面积（step=2 采样，控制耗时）
pub fn ink_fill_ratio(w: usize, h: usize, alpha: &[u8], e: &Ellipse) -> f64 {
    let (ct, st) = (e.theta_deg.to_radians().cos(), e.theta_deg.to_radians().sin());
    let (mut ink, mut area) = (0u64, 0u64);
    let step = 2usize;
    let mut y = 0usize;
    while y < h {
        let mut x = 0usize;
        while x < w {
            let dx = x as f64 - e.cx;
            let dy = y as f64 - e.cy;
            let u = dx * ct + dy * st;
            let v = -dx * st + dy * ct;
            if (u / e.a).powi(2) + (v / e.b).powi(2) <= 1.0 {
                area += 1;
                if alpha[y * w + x] > 40 { ink += 1; }
            }
            x += step;
        }
        y += step;
    }
    if area == 0 { return 1.0; }
    ink as f64 / area as f64
}

pub fn check(w: usize, h: usize, alpha: &[u8], e: &Ellipse) -> GateReport {
    let mut reasons = Vec::new();
    if e.coverage < 0.60 { reasons.push(format!("coverage {:.2} < 0.60", e.coverage)); }
    if e.inlier_ratio < 0.60 { reasons.push(format!("inlier {:.2} < 0.60", e.inlier_ratio)); }
    if e.ratio < 0.55 { reasons.push(format!("ratio {:.3} < 0.55（过度椭圆）", e.ratio)); }
    let amin = 0.12 * (w.min(h)) as f64;
    let amax = 0.65 * (w.max(h)) as f64;
    if e.a < amin || e.a > amax { reasons.push(format!("半长轴 {:.0} 越界 [{:.0},{:.0}]", e.a, amin, amax)); }
    if e.b < 0.62 * e.a { reasons.push(format!("短轴/长轴 {:.2} < 0.62", e.b / e.a)); }
    let fill = ink_fill_ratio(w, h, alpha, e);
    if fill > 0.55 { reasons.push(format!("墨迹填充 {:.2} > 0.55", fill)); }
    GateReport { ok: reasons.is_empty(), reasons, ink_fill: fill }
}
