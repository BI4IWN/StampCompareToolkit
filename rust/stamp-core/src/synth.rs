// 合成章：外环 + 内环 + 五角星 + 刻度；已知压扁比与旋转角
use image::{Rgba, RgbaImage};
use std::f64::consts::PI;

pub fn make_stamp(size: u32, ratio: f64, rot_deg: f64, ink: [u8; 3]) -> RgbaImage {
    let s = size as f64;
    let mut img = RgbaImage::new(size, size);
    let c = s / 2.0;
    let (ct, st) = (rot_deg.to_radians().cos(), rot_deg.to_radians().sin());
    let r_outer = s * 0.40;
    for y in 0..size {
        for x in 0..size {
            let px = x as f64 - c;
            let py = y as f64 - c;
            // 反向旋转到章坐标
            let u = px * ct + py * st;
            let v = -px * st + py * ct;
            let (uu, vv) = (u, v / ratio);          // 压扁
            let r = (uu * uu + vv * vv).sqrt();
            let mut a = 0.0f64;
            if (r - r_outer).abs() < s * 0.016 { a = 1.0; }               // 外环
            if (r - r_outer * 0.58).abs() < s * 0.007 { a = a.max(1.0); } // 内环
            // 五角星
            let ang = vv.atan2(uu) + PI / 2.0;
            let star_r = r_outer * 0.34;
            let k = 5.0;
            let seg = (ang.rem_euclid(2.0 * PI / k) - PI / k).abs();
            let rad_at = star_r * (0.38 / (seg + 0.38)).min(1.0);
            if r < rad_at { a = 1.0; }
            // 刻度
            if r > r_outer * 0.78 && r < r_outer * 0.86 {
                let tick = (ang.rem_euclid(2.0 * PI / 40.0) - PI / 40.0).abs();
                if tick < 0.02 { a = 1.0; }
            }
            if a > 0.0 {
                img.put_pixel(x, y, Rgba([ink[0], ink[1], ink[2], (255.0 * a) as u8]));
            }
        }
    }
    img
}
