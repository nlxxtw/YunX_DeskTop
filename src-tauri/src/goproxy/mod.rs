//! GeZi 本地 Range 代理：边下边播（sidecar）。
//! 播放器只访问 http://127.0.0.1:8791/v/<id>，上游 CDN/PCS 由 goproxy 分片拉取。

use std::collections::HashMap;
use std::process::Command;
use std::sync::OnceLock;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::json;
use tauri::{AppHandle, Manager};
use tauri_plugin_shell::ShellExt;

use crate::error::{AppError, AppResult};
use crate::logger;
use crate::state::AppState;

const LISTEN: &str = "127.0.0.1:8791";
const BASE: &str = "http://127.0.0.1:8791";

static STARTED: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
static EXE_OVERRIDE: OnceLock<std::path::PathBuf> = OnceLock::new();

/// CLI / 测试：指定 goproxy.exe 绝对路径（跳过 Tauri sidecar 解析）
pub fn init_exe(path: std::path::PathBuf) {
    let _ = EXE_OVERRIDE.set(path);
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlayResult {
    pub play_url: String,
    pub id: String,
    pub player: String,
}

#[derive(Debug, Deserialize)]
struct RegisterResp {
    ok: Option<bool>,
    id: Option<String>,
    #[serde(rename = "play_url")]
    play_url: Option<String>,
}

fn log(app: &AppHandle, action: &str, msg: &str, detail: &str) {
    app.state::<AppState>()
        .log(logger::INFO, "goproxy", action, msg, detail);
}

/// 健康检查
pub async fn healthy() -> bool {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());
    match client.get(format!("{BASE}/health")).send().await {
        Ok(r) => r.status().is_success(),
        Err(_) => false,
    }
}

/// 确保 sidecar 已起来（幂等）
pub async fn ensure_running(app: &AppHandle) -> AppResult<()> {
    if healthy().await {
        return Ok(());
    }
    let lock = STARTED.get_or_init(|| tokio::sync::Mutex::new(()));
    let _g = lock.lock().await;
    if healthy().await {
        return Ok(());
    }
    spawn_sidecar(app).await?;
    for _ in 0..40 {
        if healthy().await {
            log(app, "start", "GeZi 代理已就绪", LISTEN);
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Err(AppError::Api(format!(
        "GeZi 代理启动超时（{LISTEN} 无响应）"
    )))
}

async fn spawn_sidecar(app: &AppHandle) -> AppResult<()> {
    if let Some(path) = EXE_OVERRIDE.get() {
        return spawn_exe(path).await;
    }
    let shell = app.shell();
    let cmd = shell
        .sidecar("goproxy")
        .map_err(|e| AppError::Api(format!("找不到 goproxy sidecar: {e}")))?;
    let cmd = cmd.args(["-addr", LISTEN]);
    match cmd.spawn() {
        Ok((mut rx, _child)) => {
            log(app, "spawn", "已拉起 goproxy sidecar", LISTEN);
            let app_log = app.clone();
            tauri::async_runtime::spawn(async move {
                use tauri_plugin_shell::process::CommandEvent;
                while let Some(event) = rx.recv().await {
                    if let CommandEvent::Error(e) = event {
                        log(&app_log, "stderr", "goproxy 异常", &e);
                    }
                }
            });
            Ok(())
        }
        Err(e) => Err(AppError::Api(format!("启动 goproxy 失败: {e}"))),
    }
}

/// 直接拉起 exe（CLI / 无 AppHandle 场景）
pub async fn ensure_running_exe(exe: &std::path::Path) -> AppResult<()> {
    if healthy().await {
        return Ok(());
    }
    let lock = STARTED.get_or_init(|| tokio::sync::Mutex::new(()));
    let _g = lock.lock().await;
    if healthy().await {
        return Ok(());
    }
    spawn_exe(exe).await?;
    for _ in 0..40 {
        if healthy().await {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Err(AppError::Api(format!(
        "GeZi 代理启动超时（{LISTEN} 无响应）"
    )))
}

async fn spawn_exe(path: &std::path::Path) -> AppResult<()> {
    Command::new(path)
        .args(["-addr", LISTEN])
        .spawn()
        .map_err(|e| AppError::Api(format!("启动 goproxy 失败: {e}")))?;
    Ok(())
}

/// 无 AppHandle：注册并可选拉起播放器（CLI 测速用）
pub async fn register_play_standalone(
    url: &str,
    headers: &[(String, String)],
    size: i64,
    name: &str,
    platform: &str,
    open_player: bool,
) -> AppResult<PlayResult> {
    let id = session_id(platform, name, size);
    let (concurrency, part_size, pcs_live) = tuning(platform);

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|e| AppError::Api(e.to_string()))?;
    let _ = client
        .get(format!("{BASE}/unregister/{id}"))
        .send()
        .await;

    let mut hdr_map = HashMap::new();
    for (k, v) in headers {
        hdr_map.insert(k.clone(), v.clone());
    }

    let mut body = json!({
        "id": id,
        "url": url,
        "headers": hdr_map,
        "size": size.max(0),
        "name": name,
        "concurrency": concurrency,
        "part_size": part_size,
    });
    if pcs_live > 0 {
        body["pcs_live"] = json!(pcs_live);
    }

    let resp = client
        .post(format!("{BASE}/register"))
        .json(&body)
        .send()
        .await
        .map_err(|e| AppError::Api(format!("注册播放会话失败: {e}")))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(AppError::Api(format!("goproxy register {status}: {text}")));
    }
    let reg: RegisterResp = resp.json().await?;
    let play_url = reg
        .play_url
        .filter(|s| !s.is_empty())
        .ok_or_else(|| AppError::Api("goproxy 未返回 play_url".into()))?;
    let sid = reg.id.unwrap_or(id);
    let player = if open_player {
        launch_player(&play_url)?
    } else {
        "none".into()
    };
    Ok(PlayResult {
        play_url,
        id: sid,
        player,
    })
}

/// 按平台选择并发：夸克 GeZi 多工人；百度受控活连接（避免 PCS 403）
fn tuning(platform: &str) -> (i32, i64, i32) {
    match platform {
        "quark" | "uc" => (32, 2 * 1024 * 1024, 0),
        // 百度：512KB 分片加快首包落盘（2MB 在限速线上首屏要 20s+）；
        // pcs_live=1 单活连接避免互相限速；工人数>1 仅作排队，实际并发被 gate 卡住。
        "baidu" => (4, 512 * 1024, 1),
        _ => (8, 2 * 1024 * 1024, 0),
    }
}

/// 在主链 + mirrors 里选测速最快的可 Range 地址（百度 locate 多节点时用）
pub async fn pick_playable_url(primary: &str, mirrors: &[String], ua: &str) -> String {
    let mut candidates: Vec<String> = Vec::with_capacity(1 + mirrors.len());
    if !primary.is_empty() {
        candidates.push(primary.to_string());
    }
    for m in mirrors {
        if !m.is_empty() && !candidates.iter().any(|c| c == m) {
            candidates.push(m.clone());
        }
    }
    if candidates.is_empty() {
        return primary.to_string();
    }
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(4))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());

    // 并行对每个候选拉 256KB，按吞吐选最快（超时/失败记 0）
    let sample: u64 = 256 * 1024;
    let mut futs = Vec::new();
    for u in candidates.clone() {
        let client = client.clone();
        let ua = ua.to_string();
        futs.push(async move {
            let t0 = std::time::Instant::now();
            let res = client
                .get(&u)
                .header("User-Agent", &ua)
                .header("Range", format!("bytes=0-{}", sample - 1))
                .send()
                .await;
            match res {
                Ok(resp)
                    if resp.status().is_success()
                        || resp.status() == reqwest::StatusCode::PARTIAL_CONTENT =>
                {
                    match resp.bytes().await {
                        Ok(bytes) if !bytes.is_empty() => {
                            let secs = t0.elapsed().as_secs_f64().max(0.001);
                            let speed = bytes.len() as f64 / secs;
                            Some((u, speed))
                        }
                        _ => None,
                    }
                }
                _ => None,
            }
        });
    }
    let mut best: Option<(String, f64)> = None;
    for item in futures_util::future::join_all(futs).await.into_iter().flatten() {
        if best.as_ref().map(|(_, s)| item.1 > *s).unwrap_or(true) {
            best = Some(item);
        }
    }
    best.map(|(u, _)| u).unwrap_or_else(|| candidates[0].clone())
}

fn session_id(platform: &str, name: &str, size: i64) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    name.hash(&mut h);
    size.hash(&mut h);
    format!("{}-{:x}", platform_prefix(platform), h.finish())
}

fn platform_prefix(platform: &str) -> &'static str {
    match platform {
        "baidu" => "b",
        "quark" => "q",
        "uc" => "u",
        _ => "p",
    }
}

/// 注册上游直链并返回本地播放地址
pub async fn register_play(
    app: &AppHandle,
    url: &str,
    headers: &[(String, String)],
    size: i64,
    name: &str,
    platform: &str,
) -> AppResult<PlayResult> {
    ensure_running(app).await?;
    let id = session_id(platform, name, size);
    let (concurrency, part_size, pcs_live) = tuning(platform);

    // 同 id 先注销，避免旧会话抢带宽
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|e| AppError::Api(e.to_string()))?;
    let _ = client
        .get(format!("{BASE}/unregister/{id}"))
        .send()
        .await;

    let mut hdr_map = HashMap::new();
    for (k, v) in headers {
        hdr_map.insert(k.clone(), v.clone());
    }

    let mut body = json!({
        "id": id,
        "url": url,
        "headers": hdr_map,
        "size": size.max(0),
        "name": name,
        "concurrency": concurrency,
        "part_size": part_size,
    });
    if pcs_live > 0 {
        body["pcs_live"] = json!(pcs_live);
    }

    let resp = client
        .post(format!("{BASE}/register"))
        .json(&body)
        .send()
        .await
        .map_err(|e| AppError::Api(format!("注册播放会话失败: {e}")))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(AppError::Api(format!("goproxy register {status}: {text}")));
    }
    let reg: RegisterResp = resp.json().await?;
    let play_url = reg
        .play_url
        .filter(|s| !s.is_empty())
        .ok_or_else(|| AppError::Api("goproxy 未返回 play_url".into()))?;
    let sid = reg.id.unwrap_or(id);

    log(
        app,
        "register",
        &format!("原画播放会话已就绪（{platform}）"),
        &format!("id={sid} conc={concurrency} pcs_live={pcs_live} {play_url}"),
    );

    let player = launch_player(&play_url)?;
    Ok(PlayResult {
        play_url,
        id: sid,
        player,
    })
}

/// 查找本机播放器并打开本地代理 URL
pub fn launch_player(play_url: &str) -> AppResult<String> {
    let candidates = [
        r"G:\VLC\vlc.exe",
        r"C:\Program Files\VideoLAN\VLC\vlc.exe",
        r"C:\Program Files (x86)\VideoLAN\VLC\vlc.exe",
        r"C:\Program Files\DAUM\PotPlayer\PotPlayerMini64.exe",
        r"C:\Program Files\DAUM\PotPlayer\PotPlayerMini.exe",
        r"C:\Program Files (x86)\DAUM\PotPlayer\PotPlayerMini.exe",
    ];
    for path in candidates {
        if std::path::Path::new(path).is_file() {
            Command::new(path)
                .arg(play_url)
                .spawn()
                .map_err(|e| AppError::Api(format!("启动播放器失败: {e}")))?;
            let name = std::path::Path::new(path)
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or(path);
            return Ok(name.to_string());
        }
    }
    // 回退：系统默认关联打开 http URL（可能是浏览器）
    Command::new("cmd")
        .args(["/C", "start", "", play_url])
        .spawn()
        .map_err(|e| AppError::Api(format!("无法打开播放地址: {e}")))?;
    Ok("default".into())
}

/// 视频扩展名判定（前端/命令共用逻辑）
pub fn is_video_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    matches!(
        lower.rsplit('.').next().unwrap_or(""),
        "mp4" | "mkv" | "mov" | "m4v" | "avi" | "ts" | "flv" | "webm" | "wmv" | "m2ts" | "mpeg" | "mpg"
    )
}
