// 圆心 + 半径对齐：参考章不动，待验章缩放/平移/旋转到参考章
use crate::fit::{fit_ellipse, Ellipse};

pub struct AlignResult {
    pub d_center_px: f64,
    pub radius_ratio: f64,
    pub rotate_deg: f64,
    pub scale: f64,
}

pub fn align_from_images(alpha_a: &[u8], w: usize, h: usize, alpha_b: &[u8]) -> Option<(AlignResult, Ellipse, Ellipse)> {
    let ea = fit_ellipse(w, h, alpha_a)?;
    let eb = fit_ellipse(w, h, alpha_b)?;
    // 半径比：都做过圆章校正时 k≈1
    let k = ea.a / eb.a.max(1e-6);
    // 圆心差（把 B 平移到 A 后还剩多少）
    let d = ((ea.cx - eb.cx).powi(2) + (ea.cy - eb.cy).powi(2)).sqrt();
    let mut rot = ea.theta_deg - eb.theta_deg;
    while rot <= -180.0 { rot += 360.0; }
    while rot > 180.0 { rot -= 360.0; }
    Some((AlignResult { d_center_px: d, radius_ratio: ea.a / eb.a, rotate_deg: rot, scale: k }, ea, eb))
}
