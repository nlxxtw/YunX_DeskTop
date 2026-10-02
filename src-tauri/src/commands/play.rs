//! 原画播放：aria2 高速下载（顺序灌盘）→ 本地文件缓冲够了开播放器（下到哪播到哪）。

use std::io::Read;
use std::time::Duration;

use serde_json::json;
use tauri::{AppHandle, Emitter, Manager};

use crate::aria2::{self, parse_status, rpc_call, sanitize_out_path};
use crate::error::{AppError, AppResult};
use crate::goproxy::{self, PlayResult};
use crate::logger;
use crate::models::{DownloadLink, DownloadTaskView, ShareFile};
use crate::resolve;
use crate::state::AppState;

const WARMUP_MIN: i64 = 8 * 1024 * 1024;
const WARMUP_MAX: i64 = 32 * 1024 * 1024;
const WAIT_SECS: u64 = 300;

fn warmup_bytes(total: i64) -> i64 {
    if total <= 0 {
        return WARMUP_MIN;
    }
    let n = (total as f64 * 0.03) as i64;
    n.clamp(WARMUP_MIN, WARMUP_MAX.min(total))
}

#[tauri::command]
pub async fn play_share_file(
    app: AppHandle,
    session_key: String,
    file: ShareFile,
) -> AppResult<PlayResult> {
    if file.isdir {
        return Err(AppError::Api("文件夹不能直接播放，请进入后选择视频".into()));
    }
    if !goproxy::is_video_name(&file.fname) {
        return Err(AppError::Api(format!("暂不支持播放该格式：{}", file.fname)));
    }
    let state = app.state::<AppState>();
    state.log(logger::INFO, "play", "share", "取链后高速下载并本地播放", &file.fname);
    let link = resolve::get_download_link(&state, &session_key, &file).await?;
    play_via_aria2(&app, link).await
}

#[tauri::command]
pub async fn play_personal_file(
    app: AppHandle,
    platform: String,
    file: ShareFile,
) -> AppResult<PlayResult> {
    if file.isdir {
        return Err(AppError::Api("文件夹不能直接播放".into()));
    }
    if !goproxy::is_video_name(&file.fname) {
        return Err(AppError::Api(format!("暂不支持播放该格式：{}", file.fname)));
    }
    let state = app.state::<AppState>();
    state.log(logger::INFO, "play", "personal", "取链后高速下载并本地播放", &file.fname);
    let plat = crate::models::Platform::from_key(&platform)
        .ok_or_else(|| AppError::Api("未知平台".into()))?;
    let link = crate::api::pan_files::get_personal_download_link(&state, plat, &file).await?;
    play_via_aria2(&app, link).await
}

#[tauri::command]
pub async fn play_direct_link(
    app: AppHandle,
    url: String,
    headers: Vec<(String, String)>,
    size: i64,
    name: String,
    platform: String,
) -> AppResult<PlayResult> {
    play_via_aria2(
        &app,
        DownloadLink {
            url,
            filename: name,
            size,
            headers,
            platform,
            cleanup_id: String::new(),
            mirrors: Vec::new(),
            fetch_ctx: String::new(),
        },
    )
    .await
}

async fn play_via_aria2(app: &AppHandle, link: DownloadLink) -> AppResult<PlayResult> {
    let state = app.state::<AppState>();
    let file_name = if link.filename.is_empty() {
        "play.bin".into()
    } else {
        link.filename.clone()
    };

    if let Some(path) = find_completed_local(&state, &file_name) {
        state.log(logger::INFO, "play", "reuse", "复用已下载文件直接播放", &path);
        let player = goproxy::launch_player(&path)?;
        return Ok(PlayResult {
            play_url: path,
            id: "local".into(),
            player,
        });
    }

    let task_id = aria2::enqueue_for_play(
        app,
        &link.url,
        &file_name,
        &link.headers,
        &link.platform,
        &link.cleanup_id,
        link.mirrors.clone(),
        &link.fetch_ctx,
    )
    .await?;

    let need = warmup_bytes(link.size);
    state.log(
        logger::INFO,
        "play",
        "buffer",
        &format!(
            "任务 #{task_id} 顺序高速下载中，约缓冲 {}MB 后开播",
            need / (1024 * 1024)
        ),
        &file_name,
    );
    let _ = app.emit(
        "play:buffer",
        json!({
            "taskId": task_id,
            "needMb": need / (1024 * 1024),
            "fileName": file_name,
        }),
    );

    let (path, downloaded, total) =
        wait_until_playable(app, task_id, link.size, &file_name, need).await?;

    // 确认本地文件可读且有片头，再交给播放器（避免空路径 / 乱序空洞）
    ensure_playable_file(&path, need)?;

    let pct = if total > 0 {
        (downloaded * 100 / total).clamp(0, 100)
    } else {
        0
    };

    let player = goproxy::launch_player(&path)?;
    state.log(
        logger::SUCCESS,
        "play",
        "open",
        &format!("已缓冲 {pct}%，用 {player} 打开本地文件；后台继续下载"),
        &path,
    );

    Ok(PlayResult {
        play_url: path,
        id: task_id.to_string(),
        player,
    })
}

fn find_completed_local(state: &AppState, file_name: &str) -> Option<String> {
    let name = sanitize_out_path(file_name);
    let conn = state.db.lock().ok()?;
    let path: String = conn
        .query_row(
            "SELECT save_path FROM download_task WHERE file_name = ?1 AND status = ?2 \
             AND save_path != '' ORDER BY id DESC LIMIT 1",
            rusqlite::params![name, DownloadTaskView::STATUS_COMPLETED],
            |r| r.get(0),
        )
        .ok()?;
    if std::path::Path::new(&path).is_file() {
        Some(path)
    } else {
        None
    }
}

/// 确认磁盘上已有足够连续前缀，且路径给播放器用得上。
fn ensure_playable_file(path: &str, need: i64) -> AppResult<()> {
    let p = std::path::Path::new(path);
    if !p.is_file() {
        return Err(AppError::Api(format!(
            "本地缓冲文件不存在，无法交给播放器：{path}"
        )));
    }
    let meta = std::fs::metadata(p).map_err(|e| AppError::Api(format!("无法读取缓冲文件：{e}")))?;
    let min_ok = WARMUP_MIN.min(need).max(1024 * 1024);
    if meta.len() < min_ok as u64 {
        return Err(AppError::Api(format!(
            "缓冲不足（仅 {}MB），请稍后再点播放或到下载页查看进度",
            meta.len() / (1024 * 1024)
        )));
    }
    // 读片头几个字节：全 0 说明多半是稀疏/未真正落盘
    let mut f = std::fs::File::open(p).map_err(|e| AppError::Api(format!("无法打开缓冲文件：{e}")))?;
    let mut head = [0u8; 64];
    let n = f.read(&mut head).unwrap_or(0);
    if n < 12 || head.iter().all(|b| *b == 0) {
        return Err(AppError::Api(
            "缓冲文件片头无效（可能仍在乱序分片）。请等下载更多后再播，或重新点播放".into(),
        ));
    }
    Ok(())
}

async fn wait_until_playable(
    app: &AppHandle,
    task_id: i64,
    expect_size: i64,
    file_name: &str,
    need: i64,
) -> AppResult<(String, i64, i64)> {
    let state = app.state::<AppState>();
    let deadline = std::time::Instant::now() + Duration::from_secs(WAIT_SECS);
    let mut last_log = std::time::Instant::now() - Duration::from_secs(5);
    let mut last_emit = std::time::Instant::now() - Duration::from_secs(2);

    loop {
        if std::time::Instant::now() > deadline {
            return Err(AppError::Api(format!(
                "缓冲超时（{WAIT_SECS}s），请到「下载」页查看任务 #{task_id}；速度够快后再点播放"
            )));
        }

        let (gid, status_db, save_db, total_db, done_db) = {
            let conn = state.db.lock().map_err(|_| AppError::Lock)?;
            conn.query_row(
                "SELECT gid, status, save_path, total_size, downloaded_size FROM download_task WHERE id = ?1",
                rusqlite::params![task_id],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, i32>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, i64>(3)?,
                        r.get::<_, i64>(4)?,
                    ))
                },
            )?
        };

        if status_db == DownloadTaskView::STATUS_FAILED {
            return Err(AppError::Api(format!("下载失败，无法播放（任务 #{task_id}）")));
        }

        let mut completed = done_db;
        let mut total = if expect_size > 0 { expect_size } else { total_db };
        let mut path = save_db;

        if !gid.is_empty() {
            if let Ok(v) = rpc_call(
                "aria2.tellStatus",
                vec![
                    json!(gid),
                    json!(["status", "totalLength", "completedLength", "files", "downloadSpeed"]),
                ],
            )
            .await
            {
                let st = parse_status(&v);
                completed = st.completed;
                if st.total > 0 {
                    total = st.total;
                }
                if let Some(p) = st.files.first() {
                    if !p.is_empty() {
                        path = p.clone();
                    }
                }
                if st.status == "error" {
                    return Err(AppError::Api(format!(
                        "下载出错：{}",
                        if st.error_msg.is_empty() {
                            "aria2 error".into()
                        } else {
                            st.error_msg
                        }
                    )));
                }
                if st.status == "complete" {
                    completed = st.total.max(completed);
                }
            }
        }

        if path.is_empty() {
            let settings = state.load_settings();
            path = aria2::resolve_download_dir(app, &settings)
                .join(sanitize_out_path(file_name))
                .display()
                .to_string();
        }

        let file_len = std::fs::metadata(&path).map(|m| m.len() as i64).unwrap_or(0);
        let have = completed.max(file_len);
        let done = status_db == DownloadTaskView::STATUS_COMPLETED || (total > 0 && have >= total);
        let ready = done || have >= need;

        if last_log.elapsed() >= Duration::from_secs(2) {
            last_log = std::time::Instant::now();
            let pct = if total > 0 { have * 100 / total } else { 0 };
            state.log(
                logger::INFO,
                "play",
                "buffer",
                &format!(
                    "缓冲 {pct}%（{:.1}/{:.1} MB），开播门槛 {:.1} MB",
                    have as f64 / 1048576.0,
                    total.max(1) as f64 / 1048576.0,
                    need as f64 / 1048576.0
                ),
                &format!("task=#{task_id}"),
            );
        }
        if last_emit.elapsed() >= Duration::from_secs(1) {
            last_emit = std::time::Instant::now();
            let _ = app.emit(
                "play:buffer",
                json!({
                    "taskId": task_id,
                    "haveMb": have as f64 / 1048576.0,
                    "needMb": need as f64 / 1048576.0,
                    "totalMb": total.max(1) as f64 / 1048576.0,
                    "fileName": file_name,
                }),
            );
        }

        // 必须磁盘上真有足够字节，才认定可播（completedLength 有时超前于可读前缀）
        if ready && file_len >= WARMUP_MIN.min(need).max(1) {
            return Ok((path, have, total.max(have)));
        }

        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}
