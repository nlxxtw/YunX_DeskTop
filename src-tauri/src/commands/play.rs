//! 原画边下边播：取直链 → GeZi goproxy → 本地播放器。

use tauri::{AppHandle, Manager};

use crate::error::{AppError, AppResult};
use crate::goproxy::{self, PlayResult};
use crate::logger;
use crate::models::ShareFile;
use crate::resolve;
use crate::state::AppState;

/// 分享文件：取链后边下边播
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
        return Err(AppError::Api(format!(
            "暂不支持播放该格式：{}",
            file.fname
        )));
    }
    let state = app.state::<AppState>();
    state.log(
        logger::INFO,
        "play",
        "share",
        "开始取链并原画播放",
        &file.fname,
    );
    let link = resolve::get_download_link(&state, &session_key, &file).await?;
    let ua = link
        .headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("User-Agent"))
        .map(|(_, v)| v.as_str())
        .unwrap_or("");
    let url = goproxy::pick_playable_url(&link.url, &link.mirrors, ua).await;
    goproxy::register_play(
        &app,
        &url,
        &link.headers,
        link.size,
        &link.filename,
        &link.platform,
    )
    .await
}

/// 个人云文件：取链后边下边播
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
        return Err(AppError::Api(format!(
            "暂不支持播放该格式：{}",
            file.fname
        )));
    }
    let state = app.state::<AppState>();
    state.log(
        logger::INFO,
        "play",
        "personal",
        "开始取链并原画播放",
        &file.fname,
    );
    let plat = crate::models::Platform::from_key(&platform)
        .ok_or_else(|| AppError::Api("未知平台".into()))?;
    let link = crate::api::pan_files::get_personal_download_link(&state, plat, &file).await?;
    let ua = link
        .headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("User-Agent"))
        .map(|(_, v)| v.as_str())
        .unwrap_or("");
    let url = goproxy::pick_playable_url(&link.url, &link.mirrors, ua).await;
    goproxy::register_play(
        &app,
        &url,
        &link.headers,
        link.size,
        &link.filename,
        &link.platform,
    )
    .await
}

/// 已有直链时直接播放（调试 / 扩展用）
#[tauri::command]
pub async fn play_direct_link(
    app: AppHandle,
    url: String,
    headers: Vec<(String, String)>,
    size: i64,
    name: String,
    platform: String,
) -> AppResult<PlayResult> {
    goproxy::register_play(&app, &url, &headers, size, &name, &platform).await
}
