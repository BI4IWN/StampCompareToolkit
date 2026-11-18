#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use axum::{routing::{get, post}, Json, Router, response::Html};
use tauri::Manager;
use base64::Engine as _;
use image::RgbaImage;
use serde_json::{json, Value};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;

const PORT: u16 = 18970;

mod library;

static ENGINE: std::sync::OnceLock<Option<Arc<stamp_ocr::Engine>>> = std::sync::OnceLock::new();
static INDEX: &str = include_str!("../../web/index.html");

fn decode_png(b64: &str) -> Option<RgbaImage> {
    let bytes = base64::engine::general_purpose::STANDARD.decode(b64).ok()?;
    let img = image::load_from_memory(&bytes).ok()?.to_rgba8();
    Some(img)
}

/// 只用 stamp-core 的几何能力：拟合外圈 + 去畸变，证明 Rust 内核已在进程内跑起来
fn analyze(img: &RgbaImage) -> Value {
    let t0 = Instant::now();
    let (w, h) = (img.width() as usize, img.height() as usize);
    let alpha: Vec<u8> = img.pixels().map(|p| p.0[3]).collect();
    let ellipse = stamp_core::fit::fit_ellipse(w, h, &alpha);
    let dewarp = stamp_core::dewarp::dewarp_iterative(img);
    json!({
        "engine": "rust-geometry",
        "elapsed_ms": t0.elapsed().as_millis() as u64,
        "fit": ellipse.as_ref().map(|e| json!({
            "cx": e.cx, "cy": e.cy, "a": e.a, "b": e.b,
            "theta_deg": e.theta_deg, "ratio": e.ratio,
            "coverage": e.coverage, "inlier_ratio": e.inlier_ratio, "points": e.points,
        })),
        "dewarp": dewarp.as_ref().map(|d| json!({ "rounds": d.rounds, "ratio_after": d.after.as_ref().map(|f| f.ratio) })),
    })
}



async fn index_html() -> axum::response::Response {
    use axum::response::IntoResponse;
    (
        [
            (axum::http::header::CONTENT_TYPE, "text/html; charset=utf-8"),
            (axum::http::header::CACHE_CONTROL, "no-store"),
        ],
        index_body(),
    )
        .into_response()
}

fn index_body() -> String {
    // 调试钩子：STAMP_AUTOIMG=/path/a.png 时在页面里注入自动加载指令（用于真机复现）
    match std::env::var("STAMP_AUTOIMG") {
        Ok(p) if !p.is_empty() => {
            let tag = format!(
                "<script>window.__AUTOIMG__={:?};</script>",
                p.replace('\\', "\\\\").replace('"', "\\\"")
            );
            INDEX.replacen("</body>", &format!("{tag}</body>"), 1)
        }
        _ => INDEX.to_string(),
    }
}



// 圆章校正交给 Rust：WKWebView 与 Chromium 的 canvas 采样有差异，几何放后端结果可复现
async fn dewarp(
    axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>,
    body: axum::body::Bytes,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let w: u32 = q.get("w").and_then(|v| v.parse().ok()).unwrap_or(0);
    let h: u32 = q.get("h").and_then(|v| v.parse().ok()).unwrap_or(0);
    if w == 0 || h == 0 || body.len() != (w * h * 4) as usize {
        return (axum::http::StatusCode::BAD_REQUEST, "bad payload").into_response();
    }
    let img = match image::RgbaImage::from_raw(w, h, body.to_vec()) {
        Some(i) => i,
        None => return (axum::http::StatusCode::BAD_REQUEST, "bad buffer").into_response(),
    };
    let force = q.get("force").map(|v| v == "1").unwrap_or(false);
    let result = if force {
        // 诊断用：跳过闸门，直接按拟合椭圆拉一次
        stamp_core::fit::fit_ellipse(img.width() as usize, img.height() as usize, &img.as_raw().chunks(4).map(|p| p[3]).collect::<Vec<u8>>())
            .map(|ell| {
                let warped = stamp_core::dewarp::dewarp_once(&img, &ell);
                stamp_core::dewarp::DewarpResult { image: warped, before: ell.clone(), after: None, rounds: 1, radius: 0.0 }
            })
    } else {
        stamp_core::dewarp::dewarp_iterative(&img)
    };
    match result {
        Some(res) => {
            let mut out = std::io::Cursor::new(Vec::new());
            if image::DynamicImage::ImageRgba8(res.image)
                .write_to(&mut out, image::ImageFormat::Png)
                .is_err()
            {
                return (axum::http::StatusCode::INTERNAL_SERVER_ERROR, "encode fail").into_response();
            }
            (
                [
                    (axum::http::header::CONTENT_TYPE, "image/png"),
                    (axum::http::header::HeaderName::from_static("x-warp-before"), "1"),
                ],
                out.into_inner(),
            )
                .into_response()
        }
        None => (axum::http::StatusCode::NO_CONTENT, "no warp").into_response(),
    }
}


// ---------- 印章库 / 设置（P3-2：把 Electron 的 IPC 搬到 Rust） ----------
async fn api_settings_get() -> Json<Value> { Json(library::settings_json()) }
async fn api_settings_set(Json(body): Json<Value>) -> Json<Value> { Json(library::update_settings(&body)) }
async fn api_settings_reset() -> Json<Value> { Json(library::reset_settings()) }
async fn api_lib_list() -> Json<Value> { Json(library::lib_list()) }
async fn api_lib_search(axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>) -> Json<Value> {
    Json(library::lib_search(q.get("q").map(|s| s.as_str()).unwrap_or("")))
}
async fn api_lib_load(axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>) -> Json<Value> {
    Json(library::lib_load(q.get("id").map(|s| s.as_str()).unwrap_or("")))
}
async fn api_lib_save(Json(body): Json<Value>) -> Json<Value> { Json(library::lib_save(&body)) }
async fn api_lib_delete(Json(body): Json<Value>) -> Json<Value> {
    Json(library::lib_delete(body.get("id").and_then(|v| v.as_str()).unwrap_or("")))
}
async fn api_lib_rename(Json(body): Json<Value>) -> Json<Value> { Json(library::lib_rename(&body)) }
async fn api_lib_reveal() -> Json<Value> { Json(library::lib_reveal()) }


// ---------- 截图（用系统 screencapture 的框选界面，等价 Electron 的覆盖层选择） ----------
async fn api_capture_start() -> Json<Value> {
    let id = format!("{}", std::process::id() as u64 * 1000 + (std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.subsec_millis() as u64).unwrap_or(0)));
    let path = format!("/tmp/stamp-capture-{id}.png");
    let _ = std::fs::remove_file(&path);
    match std::process::Command::new("screencapture").arg("-i").arg("-x").arg(&path).spawn() {
        Ok(_) => Json(json!({"ok": true, "id": id, "path": path})),
        Err(e) => Json(json!({"ok": false, "error": format!("无法启动截图：{e}")})),
    }
}

async fn api_capture_poll(axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>) -> Json<Value> {
    let path = q.get("path").cloned().unwrap_or_default();
    if path.is_empty() { return Json(json!({"done": false})); }
    match std::fs::metadata(&path) {
        Ok(m) if m.len() > 1024 => match std::fs::read(&path) {
            Ok(bytes) => {
                let _ = std::fs::remove_file(&path);
                let b64 = base64::engine::general_purpose::STANDARD.encode(bytes);
                Json(json!({"done": true, "png": format!("data:image/png;base64,{b64}")}))
            }
            Err(_) => Json(json!({"done": false})),
        },
        _ => Json(json!({"done": false})),
    }
}


async fn api_lib_choose_path() -> Json<Value> {
    // 用系统「选择文件夹」对话框（AppleScript），返回 {ok, path}
    let script = r#"POSIX path of (choose folder with prompt "选择印章库文件夹")"#;
    match std::process::Command::new("osascript").arg("-e").arg(script).output() {
        Ok(o) if o.status.success() => {
            let path = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if path.is_empty() { return Json(json!({"ok": false, "canceled": true})); }
            let mut body = serde_json::Map::new();
            body.insert("libPath".into(), json!(path));
            Json(library::update_settings(&Value::Object(body)))
        }
        Ok(_) => Json(json!({"ok": false, "canceled": true})),
        Err(e) => Json(json!({"ok": false, "error": format!("无法打开选择框：{e}")})),
    }
}

async fn diagimg() -> axum::response::Response {
    use axum::response::IntoResponse;
    match std::env::var("STAMP_AUTOIMG").ok().and_then(|p| std::fs::read(p).ok()) {
        Some(bytes) => ([(axum::http::header::CONTENT_TYPE, "image/png")], bytes).into_response(),
        None => (axum::http::StatusCode::NOT_FOUND, "no image").into_response(),
    }
}


// 极简探针通道：GET /diagp?m=<任意文本>，直接追加到 /tmp/stamp-diag.log。
// 用 <img> 或 fetch(no-cors) 都能打进来，绕开 WKWebView 上 POST JSON 的静默失败。
async fn diagp(axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>) -> &'static str {
    use std::io::Write;
    let msg = q.get("m").cloned().unwrap_or_default();
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open("/tmp/stamp-diag.log") {
        let _ = writeln!(f, "{msg}");
    }
    "ok"
}

async fn diag(body: String) -> &'static str {
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true).append(true).open("/tmp/stamp-diag.log")
    {
        let _ = writeln!(f, "{body}");
    }
    "ok"
}

async fn health() -> Json<Value> {
    Json(json!({ "status": "ok", "version": "0.4.3", "ocr": true, "engine": "rust" }))
}

async fn ocr(Json(body): Json<Value>) -> Json<Value> {
    let b64 = body.get("image").and_then(|v| v.as_str()).unwrap_or("");
    let img = match decode_png(b64) {
        Some(i) => i,
        None => return Json(json!({ "error": "image decode failed" })),
    };
    let eng = ENGINE.get().cloned().flatten();
    if let Some(eng) = eng {
        let t0 = Instant::now();
        match eng.recognize(&img) {
            Ok(r) => {
                let text_for_rank = r.text.clone();
                let cands = library::rank_candidates(&text_for_rank, 3);
                let top = cands.first().and_then(|c| c.get("score")).and_then(|v| v.as_f64()).unwrap_or(0.0);
                // 库名兜底：OCR 文本与库里某条足够接近时，直接用库名（等价 Electron 的 library 覆盖）
                let cand_name = cands.first().and_then(|c| c.get("name")).and_then(|v| v.as_str()).map(|s| s.to_string());
                let (company, match_source) = match (&r.company, &cand_name, top) {
                    (_, Some(n), t) if t >= 0.50 => (Some(n.clone()), "library"),
                    (c, _, _) => (c.clone(), "ocr"),
                };
                // 前端契约：必须带 success:true，texts 是 [{text}] 对象数组
                let texts_obj: Vec<Value> = r.texts.iter().map(|t| json!({ "text": t })).collect();
                return Json(json!({
                    "success": true,
                    "engine": "rust-paddleocr-vl-rec",
                    "elapsed_ms": t0.elapsed().as_millis() as u64,
                    "stamps": [{
                        "texts": texts_obj, "full_text": r.text, "raw_text": r.text,
                        "company": company, "type": r.kind, "code": r.code, "match_source": match_source,
                        "confidence": top, "method": "rust-ring",
                        "candidates": cands, "needs_review": top < 0.72,
                        "warp_ratio": r.warp_ratio, "radius": r.radius,
                        "engine_ms": r.elapsed_ms
                    }]
                }));
            }
            Err(e) => return Json(json!({ "engine": "rust", "error": format!("{e}") })),
        }
    }
    let base = analyze(&img);
    // 结构对齐 Python /ocr，前端不会因缺字段崩掉
    Json(json!({
        "engine": "rust",
        "elapsed_ms": base.get("elapsed_ms").cloned().unwrap_or(json!(0)),
        "stamps": [ { "texts": [], "full_text": "", "company": null, "type": null, "code": null,
                      "confidence": 0, "method": "rust-geometry", "candidates": [], "needs_review": true,
                      "detail": base } ]
    }))
}

fn main() {
    tauri::Builder::default()
        .setup(|app| {
            // 模型目录：依次尝试 环境变量 / resource_dir / 可执行文件同级 / 上层 Resources / cwd
            let mut cands: Vec<std::path::PathBuf> = Vec::new();
            if let Ok(v) = std::env::var("STAMP_MODELS") { cands.push(v.into()); }
            if let Ok(p) = app.path().resource_dir() { cands.push(p.join("models")); }
            if let Ok(exe) = std::env::current_exe() {
                if let Some(d) = exe.parent() {
                    cands.push(d.join("models"));
                    if let Some(up) = d.parent() {
                        cands.push(up.join("Resources").join("models"));
                        cands.push(up.join("models"));
                    }
                }
            }
            cands.push(std::path::PathBuf::from("models"));
            let dir = cands
                .iter()
                .find(|p| p.join("rec_batch1.onnx").exists())
                .cloned()
                .unwrap_or_else(|| std::path::PathBuf::from("models"));
            eprintln!("[engine] models dir = {:?} (candidates: {:?})", dir, cands);
            let loaded = match stamp_ocr::Engine::load(&dir) {
                Ok(e) => Some(Arc::new(e)),
                Err(err) => { eprintln!("[engine] load failed dir={:?}: {err}", dir); None }
            };
            let _ = ENGINE.set(loaded);
            tauri::async_runtime::spawn(async move {
                let app = Router::new()
                    .route("/", get(index_html))
                    .route("/stamp-compare.html", get(index_html))
                    .route("/health", get(health))
                    .route("/ocr", post(ocr))
                    .route("/diag", post(diag))
                    .route("/ai/test", post(api_ai_test))
                    .route("/ai/stamp", post(api_ai_stamp))
                    .route("/ai-ocr", post(api_ai_ocr))
                    .route("/ai/ocr", post(api_ai_ocr))
                    .route("/diagp", get(diagp))
                    .route("/diagimg", get(diagimg))
                    .route("/settings", get(api_settings_get).post(api_settings_set))
                    .route("/settings/reset", post(api_settings_reset))
                    .route("/lib/list", get(api_lib_list))
                    .route("/lib/search", get(api_lib_search))
                    .route("/lib/load", get(api_lib_load))
                    .route("/lib/save", post(api_lib_save))
                    .route("/lib/delete", post(api_lib_delete))
                    .route("/lib/rename", post(api_lib_rename))
                    .route("/lib/reveal", post(api_lib_reveal))
                    .route("/lib/choose-path", post(api_lib_choose_path))
                    .route("/capture/start", post(api_capture_start))
                    .route("/capture/poll", get(api_capture_poll))
                    .route("/dewarp", post(dewarp).layer(axum::extract::DefaultBodyLimit::max(32 * 1024 * 1024)))
                    .layer(tower_http::cors::CorsLayer::permissive());
                let addr = SocketAddr::from(([127, 0, 0, 1], PORT));
                let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
                axum::serve(listener, app).await.unwrap();
            });
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

// ---------- AI 代理（OpenAI 兼容）：/ai/test、/ai/stamp、/ai-ocr ----------
fn ai_chat(cfg: &Value, content: Value, max_tokens: u32) -> Result<Value, String> {
    let base = cfg.get("base_url").and_then(|v| v.as_str()).unwrap_or("").trim_end_matches('/').to_string();
    let key = cfg.get("api_key").and_then(|v| v.as_str()).unwrap_or("");
    let model = cfg.get("model").and_then(|v| v.as_str()).unwrap_or("");
    if base.is_empty() || key.is_empty() || model.is_empty() {
        return Err("AI 配置不完整：需要 API 地址、模型与 API Key".into());
    }
    if !(base.starts_with("http://") || base.starts_with("https://")) {
        return Err(format!("API 地址不合法：{base}"));
    }
    let url = format!("{base}/chat/completions");
    let body = json!({
        "model": model,
        "messages": [{ "role": "user", "content": content }],
        "max_tokens": max_tokens,
        "temperature": 0
    });
    let resp = ureq::post(&url)
        .set("Authorization", &format!("Bearer {key}"))
        .set("Content-Type", "application/json")
        .timeout(std::time::Duration::from_secs(60))
        .send_json(body);
    match resp {
        Ok(r) => r.into_json::<Value>().map_err(|e| format!("响应解析失败：{e}")),
        Err(ureq::Error::Status(code, r)) => {
            let text = r.into_string().unwrap_or_default();
            Err(format!("HTTP {code}：{}", text.chars().take(180).collect::<String>()))
        }
        Err(e) => Err(format!("网络错误：{e}")),
    }
}

fn text_of(v: &Value) -> String {
    v.get("choices").and_then(|c| c.get(0))
        .and_then(|c| c.get("message")).and_then(|m| m.get("content"))
        .and_then(|c| c.as_str()).unwrap_or("").to_string()
}

/// 把上游的"模型不支持图片"类 400 翻成人话，避免用户看到 Unknown variant `image_url`
fn friendly_ai_error(msg: &str) -> String {
    let low = msg.to_lowercase();
    if low.contains("unknown variant `image_url`")
        || low.contains("image_url")
        || low.contains("does not support image")
        || low.contains("not support image")
        || low.contains("multimodal")
    {
        return "当前模型不支持图片输入（DeepSeek 只有文本模型）。请把服务商换成「智谱 AI（glm-4v-flash）」或「火山方舟（豆包视觉）」；若只想用 DeepSeek，请把提取方式切成「文本解析」。"
            .to_string();
    }
    if low.contains("model not found") || low.contains("invalid model") || low.contains("no such model") {
        return format!("模型名无效，请检查服务商与模型名（{msg}）");
    }
    if low.contains("insufficient") || low.contains("quota") || low.contains("balance") {
        return format!("账号额度不足或欠费（{msg}）");
    }
    msg.to_string()
}

fn extract_json(s: &str) -> Option<Value> {
    let a = s.find('{')?; let b = s.rfind('}')?;
    serde_json::from_str::<Value>(&s[a..=b]).ok()
}

async fn api_ai_test(Json(body): Json<Value>) -> Json<Value> {
    let cfg = body.get("config").cloned().unwrap_or(Value::Null);
    let out = tokio::task::spawn_blocking(move || ai_chat(&cfg, json!("ping"), 8)).await
        .unwrap_or_else(|e| Err(format!("线程错误：{e}")));
    match out {
        Ok(v) => Json(json!({"success": true, "model": v.get("model").cloned().unwrap_or(Value::Null), "reply": text_of(&v)})),
        Err(e) => Json(json!({"success": false, "error": friendly_ai_error(&e)})),
    }
}

async fn api_ai_stamp(Json(body): Json<Value>) -> Json<Value> {
    let cfg = body.get("config").cloned().unwrap_or(Value::Null);
    let image = body.get("image").and_then(|v| v.as_str()).unwrap_or("").to_string();
    if image.is_empty() { return Json(json!({"success": false, "error": "缺少 image"})); }
    let out = tokio::task::spawn_blocking(move || {
        let content = json!([
            { "type": "text", "text": "定位图中印章：给出圆心与半径（相对图片宽高的 0~1 归一化）。只返回 JSON：{\"cx\":0.5,\"cy\":0.5,\"r\":0.3}" },
            { "type": "image_url", "image_url": { "url": image, "detail": "high" } }
        ]);
        ai_chat(&cfg, content, 200)
    }).await.unwrap_or_else(|e| Err(format!("线程错误：{e}")));
    match out {
        Ok(v) => {
            let text = text_of(&v);
            match extract_json(&text) {
                Some(j) => Json(json!({"success": true, "stamp": {
                    "cx": j.get("cx").and_then(|x| x.as_f64()).unwrap_or(0.5),
                    "cy": j.get("cy").and_then(|x| x.as_f64()).unwrap_or(0.5),
                    "r":  j.get("r").and_then(|x| x.as_f64()).unwrap_or(0.3),
                    "raw": text
                }})),
                None => Json(json!({"success": false, "error": "AI 未返回可解析的 JSON", "raw": text})),
            }
        }
        Err(e) => Json(json!({"success": false, "error": friendly_ai_error(&e)})),
    }
}

async fn api_ai_ocr(Json(body): Json<Value>) -> Json<Value> {
    let cfg = body.get("config").cloned().unwrap_or(Value::Null);
    let image = body.get("image").and_then(|v| v.as_str()).unwrap_or("").to_string();
    if image.is_empty() { return Json(json!({"success": false, "error": "缺少 image"})); }
    let model_name = cfg.get("model").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let out = tokio::task::spawn_blocking(move || {
        let content = json!([
            { "type": "text", "text": "读这枚印章：先判朝向（必要时转 180°），裁出印章，沿圆周读字。只返回 JSON：{\"company\":\"\",\"code\":\"\",\"type\":\"\"}" },
            { "type": "image_url", "image_url": { "url": image, "detail": "high" } }
        ]);
        ai_chat(&cfg, content, 400)
    }).await.unwrap_or_else(|e| Err(format!("线程错误：{e}")));
    match out {
        Ok(v) => {
            let text = text_of(&v);
            match extract_json(&text) {
                Some(j) => Json(json!({
                    "success": true, "mode": "vision", "model": model_name,
                    "ai": {
                        "company": j.get("company").and_then(|x| x.as_str()).unwrap_or(""),
                        "code": j.get("code").and_then(|x| x.as_str()).unwrap_or(""),
                        "type": j.get("type").and_then(|x| x.as_str()).unwrap_or(""),
                        "raw": text
                    }
                })),
                None => Json(json!({"success": false, "error": "AI 未返回可解析的 JSON", "raw": text})),
            }
        }
        Err(e) => Json(json!({"success": false, "error": friendly_ai_error(&e)})),
    }
}
