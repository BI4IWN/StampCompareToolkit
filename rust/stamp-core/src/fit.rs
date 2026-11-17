// 椭圆拟合：边界射线取样 + 二阶矩 + 射线偏置反查（对齐 JS 版参数）
use std::f64::consts::PI;

#[derive(Clone, Copy, Debug)]
pub struct Pt { pub x: f64, pub y: f64 }

#[derive(Clone, Debug)]
pub struct Ellipse {
    pub cx: f64, pub cy: f64,
    pub a: f64, pub b: f64,
    pub ratio: f64,
    pub theta_deg: f64,
    pub coverage: f64,
    pub inlier_ratio: f64,
    pub points: usize,
}

fn median(v: &mut Vec<f64>) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n = v.len();
    if n == 0 { return 0.0; }
    if n % 2 == 1 { v[n / 2] } else { (v[n / 2 - 1] + v[n / 2]) / 2.0 }
}

/// alpha>40 的质心
pub fn mask_centroid(w: usize, h: usize, alpha: &[u8]) -> (f64, f64) {
    let (mut sx, mut sy, mut n) = (0.0f64, 0.0f64, 0u64);
    for y in 0..h { for x in 0..w {
        if alpha[y * w + x] > 40 { sx += x as f64; sy += y as f64; n += 1; }
    }}
    if n == 0 { return (w as f64 / 2.0, h as f64 / 2.0); }
    (sx / n as f64, sy / n as f64)
}

/// 720 条射线取最外层边界点，只保留 r >= 0.86*median(r)
pub fn boundary_points(w: usize, h: usize, alpha: &[u8], cx: f64, cy: f64) -> Vec<Pt> {
    let rays = 720usize;
    // 射线起点取"掩膜最远点 + 2px"，避免固定比例在小图上越界/在大图上扫空
    let mut far = 0.0f64;
    for y in 0..h { for x in 0..w {
        if alpha[y * w + x] > 40 {
            let d = ((x as f64 - cx).powi(2) + (y as f64 - cy).powi(2)).sqrt();
            if d > far { far = d; }
        }
    }}
    let rmax = (far + 2.0).min((w.max(h)) as f64);
    let mut pts: Vec<Pt> = Vec::with_capacity(rays);
    for i in 0..rays {
        let ang = i as f64 * 2.0 * PI / rays as f64;
        let (dx, dy) = (ang.cos(), ang.sin());
        let mut r = rmax;
        let mut found: Option<f64> = None;
        // 由外向内找第一个 alpha>40 的像素（与 JS 的"最外点"一致）
        while r > 1.0 {
            let x = cx + dx * r;
            let y = cy + dy * r;
            if x >= 0.0 && y >= 0.0 && (x as usize) < w && (y as usize) < h {
                if alpha[(y as usize) * w + (x as usize)] > 40 { found = Some(r); break; }
            }
            r -= 1.0;
        }
        if let Some(r) = found { pts.push(Pt { x: cx + dx * r, y: cy + dy * r }); }
    }
    let mut rs: Vec<f64> = pts.iter().map(|p| ((p.x - cx).powi(2) + (p.y - cy).powi(2)).sqrt()).collect();
    let med = median(&mut rs);
    pts.into_iter().filter(|p| ((p.x - cx).powi(2) + (p.y - cy).powi(2)).sqrt() >= 0.86 * med).collect()
}

/// 射线取样会把轴比往 1 拉：解析求"估计轴比"，再二分反查真值
fn ray_ratio_hat(rho: f64) -> f64 {
    let n = 240;
    let (a, b) = (1.0f64, rho);
    let (mut sx, mut sy, mut n_ok) = (0.0f64, 0.0f64, 0usize);
    for i in 0..n {
        let th = i as f64 * 2.0 * PI / n as f64;
        let (dx, dy) = (th.cos(), th.sin());
        // 与本程序一致的边界交点求解（椭圆参数化求 r）
        let denom = ((dx * dx) / (a * a) + (dy * dy) / (b * b)).sqrt();
        if denom <= 0.0 { continue; }
        let r = 1.0 / denom;
        sx += (dx * r) * (dx * r);
        sy += (dy * r) * (dy * r);
        n_ok += 1;
    }
    if n_ok == 0 { return 1.0; }
    let (m20, m02) = (sx / n_ok as f64, sy / n_ok as f64);
    let (ra, rb) = (2.0 * m20.sqrt(), 2.0 * m02.sqrt());
    if ra <= 0.0 { return 1.0; }
    rb / ra
}

fn true_ratio_from_ray_ratio(rh: f64) -> f64 {
    let mut lo = 0.30f64;
    let mut hi = 1.0f64;
    for _ in 0..40 {
        let mid = (lo + hi) / 2.0;
        if ray_ratio_hat(mid) < rh { lo = mid; } else { hi = mid; }
    }
    (lo + hi) / 2.0
}

/// 二阶矩拟合（4 轮残差剔除）
pub fn fit_ellipse(w: usize, h: usize, alpha: &[u8]) -> Option<Ellipse> {
    let (cx0, cy0) = mask_centroid(w, h, alpha);
    let mut pts = boundary_points(w, h, alpha, cx0, cy0);
    if pts.len() < 20 { return None; }

    let mut cx = cx0; let mut cy = cy0;
    let mut best = (1.0f64, 1.0f64, 0.0f64, 1.0f64, 0usize, 1.0f64); // a,b,theta,inlier,pts,rho
    for _round in 0..4 {
        let n = pts.len() as f64;
        let (mut m20, mut m02, mut m11) = (0.0f64, 0.0f64, 0.0f64);
        for p in &pts { let (x, y) = (p.x - cx, p.y - cy); m20 += x * x; m02 += y * y; m11 += x * y; }
        m20 /= n; m02 /= n; m11 /= n;
        let tr = m20 + m02;
        let det = m20 * m02 - m11 * m11;
        let disc = (tr * tr / 4.0 - det).max(0.0).sqrt();
        let l1 = tr / 2.0 + disc;
        let l2 = (tr / 2.0 - disc).max(1e-9);
        // 边界点均匀分布：var = a^2/2 → a = sqrt(2*lambda)
        let a_raw = (2.0 * l1).sqrt();
        let b_raw = (2.0 * l2).sqrt();
        // 长轴方向：比较两轴方差（避免 ±45° 二义性）
        let theta = 0.5 * (2.0 * m11).atan2(m20 - m02);
        let theta = if m20 >= m02 { theta } else { theta + PI / 2.0 };
        let rh = (b_raw / a_raw).clamp(0.05, 1.0);
        let rho = true_ratio_from_ray_ratio(rh);

        // 残差剔除：归一化半径偏差 <= 0.16
        let (ct, st) = (theta.cos(), theta.sin());
        let keep: Vec<Pt> = pts.iter().copied().filter(|p| {
            let (x, y) = (p.x - cx, p.y - cy);
            let u = x * ct + y * st;
            let v = -x * st + y * ct;
            let rad = ((u / a_raw).powi(2) + (v / b_raw).powi(2)).sqrt();
            (rad - 1.0).abs() <= 0.16
        }).collect();
        let inlier_ratio = keep.len() as f64 / pts.len() as f64;
        best = (a_raw, b_raw, theta, inlier_ratio, pts.len(), rho);
        if keep.len() < 20 { break; }
        // 用内点重新定中心，再拟合
        let (mut sx, mut sy) = (0.0, 0.0);
        for p in &keep { sx += p.x; sy += p.y; }
        cx = sx / keep.len() as f64;
        cy = sy / keep.len() as f64;
        pts = keep;
    }

    let (a, b, theta, inl, np, rho) = best;
    // 角度覆盖：72 个 5° 分箱
    let mut occ = [false; 72];
    for p in &pts {
        let ang = (p.y - cy).atan2(p.x - cx);
        let mut idx = ((ang + 2.0 * PI) % (2.0 * PI) / (2.0 * PI) * 72.0) as isize;
        if idx >= 72 { idx = 71; }
        occ[idx as usize] = true;
    }
    let coverage = occ.iter().filter(|v| **v).count() as f64 / 72.0;
    let ratio = rho;   // 射线偏置反查后的真实轴比
    Some(Ellipse { cx, cy, a, b, ratio, theta_deg: theta.to_degrees(), coverage, inlier_ratio: inl, points: np })
}
