//! 印章库 + 设置：把 Electron 的 lib:*/settings:* IPC 搬到 Rust，
//! 由 HTTP 路由暴露给内置页面（页面侧用 stampDesktop shim 调用）。
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Serialize, Deserialize, Clone, Default)]
pub struct Settings {
    #[serde(rename = "libPath", default)]
    pub lib_path: String,
    #[serde(rename = "captureShortcut", default)]
    pub capture_shortcut: String,
    #[serde(rename = "pasteTargetAfterCapture", default)]
    pub paste_target: String,
    #[serde(rename = "autoStartBackend", default)]
    pub auto_start_backend: String,
    #[serde(default)]
    pub version: String,
}

impl Settings {
    fn defaults() -> Self {
        Settings {
            lib_path: default_lib_path(),
            capture_shortcut: "CommandOrControl+Shift+S".into(),
            paste_target: "ask".into(),
            auto_start_backend: "true".into(),
            version: env!("CARGO_PKG_VERSION").into(),
        }
    }
}

fn app_home() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join("Library/Application Support/印章比对工具")
}

fn default_lib_path() -> String {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join("Downloads/印章库").to_string_lossy().into_owned()
}

fn settings_file() -> PathBuf { app_home().join("settings.json") }
fn index_file(dir: &Path) -> PathBuf { dir.join("index.json") }

pub fn load_settings() -> Settings {
    let p = settings_file();
    let mut s = fs::read_to_string(&p).ok()
        .and_then(|t| serde_json::from_str::<Settings>(&t).ok())
        .unwrap_or_else(Settings::defaults);
    if s.lib_path.trim().is_empty() { s.lib_path = default_lib_path(); }
    if s.capture_shortcut.trim().is_empty() { s.capture_shortcut = "CommandOrControl+Shift+S".into(); }
    s.version = env!("CARGO_PKG_VERSION").into();
    s
}

pub fn save_settings(s: &Settings) {
    let _ = fs::create_dir_all(app_home());
    if let Ok(t) = serde_json::to_string_pretty(s) { let _ = fs::write(settings_file(), t); }
}

pub fn settings_json() -> Value {
    let s = load_settings();
    json!({
        "libPath": s.lib_path, "captureShortcut": s.capture_shortcut,
        "pasteTargetAfterCapture": s.paste_target, "autoStartBackend": s.auto_start_backend,
        "version": s.version, "platform": std::env::consts::OS,
    })
}

pub fn update_settings(patch: &Value) -> Value {
    let mut s = load_settings();
    if let Some(v) = patch.get("libPath").and_then(|v| v.as_str()) { s.lib_path = v.to_string(); }
    if let Some(v) = patch.get("captureShortcut").and_then(|v| v.as_str()) { s.capture_shortcut = v.to_string(); }
    if let Some(v) = patch.get("pasteTargetAfterCapture").and_then(|v| v.as_str()) { s.paste_target = v.to_string(); }
    if let Some(v) = patch.get("autoStartBackend").and_then(|v| v.as_str()) { s.auto_start_backend = v.to_string(); }
    save_settings(&s);
    settings_json()
}

pub fn reset_settings() -> Value {
    let s = Settings::defaults();
    save_settings(&s);
    settings_json()
}

fn read_index(dir: &Path) -> Value {
    fs::read_to_string(index_file(dir)).ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .filter(|v| v.get("seals").map(|s| s.is_array()).unwrap_or(false))
        .unwrap_or_else(|| json!({ "seals": [] }))
}

fn write_index(dir: &Path, idx: &Value) {
    let _ = fs::create_dir_all(dir);
    if let Ok(t) = serde_json::to_string_pretty(idx) { let _ = fs::write(index_file(dir), t); }
}

/// 目录里手工放进去的图片自动补进索引（与 Electron 的 syncLibIndex 同义）
fn sync_index(dir: &Path) {
    let Ok(rd) = fs::read_dir(dir) else { return };
    let mut idx = read_index(dir);
    let known: Vec<String> = idx["seals"].as_array().map(|a| a.iter()
        .filter_map(|s| s.get("file").and_then(|f| f.as_str()).map(|x| x.to_string())).collect())
        .unwrap_or_default();
    let mut changed = false;
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let lower = name.to_lowercase();
        if !(lower.ends_with(".png") || lower.ends_with(".jpg") || lower.ends_with(".jpeg")) { continue; }
        if name.ends_with("_原图.png") || known.contains(&name) { continue; }
        let stem = name.rsplit_once('.').map(|(a, _)| a.to_string()).unwrap_or(name.clone());
        let clean = strip_suffix_id(&stem);
        let saved_at = e.metadata().ok().and_then(|m| m.modified().ok())
            .map(|t| chrono_like(t)).unwrap_or_else(|| "".into());
        idx["seals"].as_array_mut().unwrap().push(json!({
            "id": format!("import_{}", now_id()),
            "name": clean, "note": "目录导入", "file": name,
            "originalFile": null, "ocr": null, "savedAt": saved_at,
        }));
        changed = true;
    }
    if changed { write_index(dir, &idx); }
}

fn strip_suffix_id(stem: &str) -> String {
    // 去掉 `_<13位时间戳>_<随机>` 尾巴
    let mut parts: Vec<&str> = stem.rsplitn(3, '_').collect(); // [rand, ts, rest]
    if parts.len() == 3 && parts[0].len() >= 4 && parts[1].len() == 13 && parts[1].chars().all(|c| c.is_ascii_digit()) {
        parts.remove(0); parts.remove(0);
        return parts.join("_");
    }
    stem.to_string()
}

fn now_id() -> String {
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    format!("{}_{}", t.as_millis(), t.subsec_nanos() % 100000)
}

fn chrono_like(t: std::time::SystemTime) -> String {
    let secs = t.duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    format!("{}", secs)
}

fn safe_name(name: &str) -> String {
    let cleaned: String = name.chars().map(|c| if "\\/:*?\"<>|\n\r".contains(c) { '_' } else { c }).collect();
    let trimmed = cleaned.trim();
    let s = if trimmed.is_empty() { "未命名印章" } else { trimmed };
    s.chars().take(80).collect()
}

fn decode_data_url(s: &str) -> Option<Vec<u8>> {
    let b64 = s.split_once(',').map(|(_, b)| b).unwrap_or(s);
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.decode(b64).ok()
}

pub fn lib_save(body: &Value) -> Value {
    let dir = PathBuf::from(load_settings().lib_path);
    let _ = fs::create_dir_all(&dir);
    let mut idx = read_index(&dir);
    let name = safe_name(body.get("name").and_then(|v| v.as_str()).unwrap_or(""));
    let id = now_id();
    let stamp_file = format!("{}_{}.png", name, id);
    let Some(png) = body.get("png").and_then(|v| v.as_str()).and_then(decode_data_url) else {
        return json!({ "ok": false, "error": "png 解析失败" });
    };
    if fs::write(dir.join(&stamp_file), png).is_err() {
        return json!({ "ok": false, "error": "写入失败" });
    }
    let mut original_file = Value::Null;
    if let Some(orig) = body.get("original").and_then(|v| v.as_str()).and_then(decode_data_url) {
        let f = format!("{}_{}_原图.png", name, id);
        if fs::write(dir.join(&f), orig).is_ok() { original_file = json!(f); }
    }
    idx["seals"].as_array_mut().unwrap().push(json!({
        "id": id, "name": name,
        "note": body.get("note").and_then(|v| v.as_str()).unwrap_or(""),
        "file": stamp_file, "originalFile": original_file,
        "ocr": body.get("ocr").cloned().unwrap_or(Value::Null),
        "savedAt": now_id(),
    }));
    write_index(&dir, &idx);
    json!({ "ok": true, "id": id })
}

/// 若条目缺 savedAt（老数据/手改坏了），用文件的创建时间兜底；改了就写回索引。
fn backfill_saved_at(dir: &PathBuf, mut idx: Value) -> Value {
    let Some(list) = idx["seals"].as_array_mut() else { return idx };
    let mut changed = false;
    for seal in list.iter_mut() {
        let cur = seal.get("savedAt").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let ok = !cur.is_empty() && (cur.chars().all(|c| c.is_ascii_digit()) || cur.contains('T'));
        if ok { continue; }
        let file = seal.get("file").and_then(|v| v.as_str()).unwrap_or("").to_string();
        if file.is_empty() { continue; }
        let path = dir.join(&file);
        let Ok(meta) = fs::metadata(&path) else { continue };
        // 优先创建时间（macOS/Windows 有），退回修改时间
        let when = meta.created().or_else(|_| meta.modified()).ok();
        if let Some(t) = when {
            seal["savedAt"] = json!(chrono_like(t));
            changed = true;
        }
    }
    if changed { let _ = write_index(dir, &idx); }
    idx
}

pub fn lib_list() -> Value {
    let dir = PathBuf::from(load_settings().lib_path);
    let _ = fs::create_dir_all(&dir);
    sync_index(&dir);
    let idx = backfill_saved_at(&dir, read_index(&dir));
    idx["seals"].clone()
}

pub fn lib_search(q: &str) -> Value {
    let all = lib_list();
    let kw = q.trim().to_lowercase();
    if kw.is_empty() { return all; }
    let hit: Vec<Value> = all.as_array().cloned().unwrap_or_default().into_iter().filter(|s| {
        let name = s.get("name").and_then(|v| v.as_str()).unwrap_or("").to_lowercase();
        let note = s.get("note").and_then(|v| v.as_str()).unwrap_or("").to_lowercase();
        let comp = s.get("ocr").and_then(|o| o.get("company")).and_then(|v| v.as_str()).unwrap_or("").to_lowercase();
        name.contains(&kw) || note.contains(&kw) || comp.contains(&kw)
    }).collect();
    Value::Array(hit)
}

pub fn lib_load(id: &str) -> Value {
    let dir = PathBuf::from(load_settings().lib_path);
    let idx = read_index(&dir);
    let Some(seal) = idx["seals"].as_array().and_then(|a| a.iter().find(|s| s.get("id").and_then(|v| v.as_str()) == Some(id))).cloned() else {
        return json!({ "ok": false, "error": "not found" });
    };
    let file = seal.get("file").and_then(|v| v.as_str()).unwrap_or("");
    let path = dir.join(file);
    let Ok(bytes) = fs::read(&path) else { return json!({ "ok": false, "error": "file missing" }) };
    let mime = if file.to_lowercase().ends_with(".jpg") || file.to_lowercase().ends_with(".jpeg") { "image/jpeg" } else { "image/png" };
    use base64::Engine as _;
    let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
    let mut original = Value::Null;
    if let Some(of) = seal.get("originalFile").and_then(|v| v.as_str()) {
        if let Ok(ob) = fs::read(dir.join(of)) {
            original = json!(format!("data:{};base64,{}", mime, base64::engine::general_purpose::STANDARD.encode(&ob)));
        }
    }
    json!({ "ok": true, "png": format!("data:{};base64,{}", mime, b64), "originalPng": original, "meta": seal })
}

pub fn lib_delete(id: &str) -> Value {
    let dir = PathBuf::from(load_settings().lib_path);
    let mut idx = read_index(&dir);
    let mut removed: Option<Value> = None;
    if let Some(arr) = idx["seals"].as_array_mut() {
        if let Some(pos) = arr.iter().position(|s| s.get("id").and_then(|v| v.as_str()) == Some(id)) {
            removed = Some(arr.remove(pos));
        }
    }
    if let Some(seal) = &removed {
        if let Some(f) = seal.get("file").and_then(|v| v.as_str()) { let _ = fs::remove_file(dir.join(f)); }
        if let Some(f) = seal.get("originalFile").and_then(|v| v.as_str()) { let _ = fs::remove_file(dir.join(f)); }
        write_index(&dir, &idx);
        json!({ "ok": true })
    } else {
        json!({ "ok": false })
    }
}

pub fn lib_rename(body: &Value) -> Value {
    let dir = PathBuf::from(load_settings().lib_path);
    let id = body.get("id").and_then(|v| v.as_str()).unwrap_or("");
    let next = safe_name(body.get("name").and_then(|v| v.as_str()).unwrap_or(""));
    let mut idx = read_index(&dir);
    let Some(pos) = idx["seals"].as_array().and_then(|a| a.iter().position(|s| s.get("id").and_then(|v| v.as_str()) == Some(id))) else {
        return json!({ "ok": false, "error": "not found" });
    };
    let cur_name = idx["seals"][pos]["name"].as_str().unwrap_or("").to_string();
    if next == cur_name { return json!({ "ok": true, "name": cur_name, "renamed": false }) }
    let rename_file = |file: &str, suffix: &str| -> String {
        // 只改本程序命名的文件：`<name>_<id>[_原图].ext`
        let stem_tail = format!("_{}{}", id, suffix);
        let lower = file.to_lowercase();
        let ext = if lower.ends_with(".jpeg") { "jpeg" } else if lower.ends_with(".jpg") { "jpg" } else if lower.ends_with(".png") { "png" } else { return file.to_string() };
        let stem = &file[..file.len() - ext.len() - 1];
        if !stem.ends_with(&stem_tail) { return file.to_string(); }
        let mut target = format!("{}{}.{}", next, stem_tail, ext);
        let mut n = 2;
        while target != file && dir.join(&target).exists() {
            target = format!("{} (2){}.{}", next, stem_tail, ext);
            n += 1;
            if n > 50 { break }
        }
        let _ = fs::rename(dir.join(file), dir.join(&target));
        target
    };
    let f0 = idx["seals"][pos]["file"].as_str().unwrap_or("").to_string();
    let new_file = rename_file(&f0, "");
    idx["seals"][pos]["file"] = json!(new_file);
    if let Some(of) = idx["seals"][pos].get("originalFile").and_then(|v| v.as_str()).map(|s| s.to_string()) {
        let nf = rename_file(&of, "_原图");
        idx["seals"][pos]["originalFile"] = json!(nf);
    }
    idx["seals"][pos]["name"] = json!(next);
    write_index(&dir, &idx);
    json!({ "ok": true, "name": next, "file": idx["seals"][pos]["file"], "renamed": true })
}

pub fn lib_reveal() -> Value {
    let dir = load_settings().lib_path;
    let _ = std::process::Command::new("open").arg(&dir).spawn();
    json!({ "ok": true, "path": dir })
}

/// 归一化：只留中文/字母/数字（用于名称比对）
fn norm_chars(s: &str) -> Vec<char> {
    s.chars().filter(|c| c.is_alphanumeric()).flat_map(|c| c.to_lowercase()).collect()
}

/// 最长公共子序列占比（对错字/漏字鲁棒）
fn lcs_ratio(a: &[char], b: &[char]) -> f64 {
    if a.is_empty() || b.is_empty() { return 0.0; }
    let (a, b) = if a.len() > 240 { (&a[..240], b) } else { (a, b) };
    let n = a.len().min(240); let m = b.len().min(240);
    let mut prev = vec![0u16; m + 1];
    let mut cur = vec![0u16; m + 1];
    for i in 1..=n {
        for j in 1..=m {
            cur[j] = if a[i - 1] == b[j - 1] { prev[j - 1] + 1 } else { prev[j].max(cur[j - 1]) };
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[m] as f64 / n as f64
}

/// 二元组重合率
fn bigram_ratio(a: &[char], b: &[char]) -> f64 {
    if a.len() < 2 || b.len() < 2 { return 0.0; }
    let mut set: std::collections::HashSet<(char, char)> = std::collections::HashSet::new();
    for w in b.windows(2) { set.insert((w[0], w[1])); }
    let hit = a.windows(2).filter(|w| set.contains(&(w[0], w[1]))).count();
    hit as f64 / (a.len() - 1) as f64
}

/// 在印章库里按名称相似度排 Top-K（与 Electron 的 rank_library_candidates 同义）
pub fn rank_candidates(text: &str, k: usize) -> Vec<Value> {
    let q = norm_chars(text);
    if q.is_empty() { return vec![]; }
    let all = lib_list();
    let mut scored: Vec<(f64, Value)> = all.as_array().cloned().unwrap_or_default().into_iter().filter_map(|s| {
        let name = s.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let comp = s.get("ocr").and_then(|o| o.get("company")).and_then(|v| v.as_str()).unwrap_or("").to_string();
        let target = if comp.chars().count() > name.chars().count() { comp.clone() } else { name.clone() };
        let t = norm_chars(&target);
        if t.is_empty() { return None; }
        let score = 0.6 * lcs_ratio(&q, &t) + 0.4 * bigram_ratio(&q, &t);
        Some((score, json!({
            "id": s.get("id").cloned().unwrap_or(Value::Null),
            "name": target,
            "score": (score * 1000.0).round() / 1000.0,
        })))
    }).collect();
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    // 同名去重（库里同一枚章常有多份扫描件），避免 Top-3 里出现三条一样的名字
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    scored.into_iter()
        .filter(|(_, v)| v.get("name").and_then(|n| n.as_str()).map(|n| seen.insert(n.to_string())).unwrap_or(false))
        .take(k).map(|(_, v)| v).collect()
}
