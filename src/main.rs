//! APK Downloader
//!
//! Google Play アプリの取得操作を簡略化する Windows 常駐ツール。
//!
//! 構成:
//! - `server`   拡張機能からの受け口 (127.0.0.1 限定 + 合言葉)
//! - `watcher`  クリップボード監視 / `app` 側でホットキー
//! - `playstore` ストアページからのアプリ情報取得
//! - `region`   国ごとの配信状況の突き合わせ
//! - `provider` APK 取得 (外部ツール apkeep を駆動)
//! - `jobs`     取得ジョブの直列実行と状態保持
//! - `app`/`ui`/`dialogs` 画面
//!
//! 条件付き (cfg_attr) にすると効かずコンソールが開くため、常に GUI モードにする。
//! 開発中の出力は `logfile` に流す。
#![windows_subsystem = "windows"]

mod app;
mod build_info;
mod config;
mod dialogs;
mod fonts;
mod history;
mod icons;
mod jobs;
mod logfile;
mod notify;
mod paths;
mod playstore;
mod provider;
mod region;
mod server;
mod tray;
mod ui;
mod watcher;

use windows::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError};
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowW, SW_RESTORE, SetForegroundWindow, ShowWindow,
};
use windows::core::HSTRING;

fn main() -> eframe::Result {
    logfile::init(paths::data_dir());
    logfile::trim_if_large();
    logfile::write(format!(
        "起動 {} v{} / データ {}",
        build_info::WINDOW_TITLE,
        build_info::VERSION,
        paths::data_dir().display()
    ));

    if !acquire_single_instance() {
        logfile::write("既に起動済み。既存ウィンドウを前面に出して終了する");
        focus_existing_window();
        return Ok(());
    }

    let settings = config::Settings::load();

    let mut viewport = egui::ViewportBuilder::default()
        .with_title(build_info::WINDOW_TITLE)
        .with_inner_size(settings.window_size.map(|(w, h)| [w, h]).unwrap_or(app::DEFAULT_SIZE))
        .with_min_inner_size(app::MIN_SIZE)
        // トレイ格納で起動する場合、最初のフレームから隠しておく。
        // 出してから消すと一瞬ちらつく。
        .with_visible(!settings.start_minimized);
    if let Some((x, y)) = settings.window_pos {
        viewport = viewport.with_position([x, y]);
    }

    let options = eframe::NativeOptions { viewport, ..Default::default() };

    eframe::run_native(
        build_info::WINDOW_TITLE,
        options,
        Box::new(move |cc| Ok(Box::new(app::App::new(cc, settings)))),
    )
}

/// 多重起動を防ぐ。名前付きミューテックスを取れたら自分が最初の 1 つ。
///
/// 常駐アプリで 2 つ動くと、受け口のポートを奪い合ってどちらも中途半端になる。
///
/// 取得したハンドルは意図的に閉じない。プロセス終了時に OS が解放する。
fn acquire_single_instance() -> bool {
    // `Global\` は SeCreateGlobalPrivilege が要る。標準ユーザーでは作れないので
    // 失敗したらセッション内で有効な `Local\` へ落とす。
    let names = [
        build_info::MUTEX_NAME.to_string(),
        build_info::MUTEX_NAME.replace(r"Global\", r"Local\"),
    ];

    for name in names {
        match unsafe { CreateMutexW(None, true, &HSTRING::from(&name)) } {
            Ok(_) => return unsafe { GetLastError() } != ERROR_ALREADY_EXISTS,
            Err(e) => logfile::write(format!("ミューテックス {name} を作成できない: {e}")),
        }
    }
    // ミューテックスが一切作れない環境では、多重起動を許してでも起動させる。
    true
}

/// 既に起動しているインスタンスのウィンドウを前面に出す。
///
/// **まず受け口越しに本人へ頼む。** トレイ格納中のウィンドウを外から `ShowWindow` で
/// 出すと winit の内部状態とずれ、そのあと ✕ を押しても隠れなくなる (実測)。
/// 受け口が使えないとき (無効化・ポート違い) だけ、直接出す方に落とす。
fn focus_existing_window() {
    if request_show() {
        return;
    }

    let title = HSTRING::from(build_info::WINDOW_TITLE);
    if let Ok(hwnd) = unsafe { FindWindowW(None, &title) }
        && !hwnd.is_invalid()
    {
        logfile::write("受け口に頼めないので直接ウィンドウを出す");
        unsafe {
            let _ = ShowWindow(hwnd, SW_RESTORE);
            let _ = SetForegroundWindow(hwnd);
        }
    }
}

/// 動いている側へ「ウィンドウを出して」と頼む。頼めたら `true`。
fn request_show() -> bool {
    let settings = config::Settings::load();
    if !settings.server_enabled {
        return false;
    }

    let client = match reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .build()
    {
        Ok(client) => client,
        Err(_) => return false,
    };

    client
        .post(format!("http://127.0.0.1:{}/show", settings.server_port))
        .header("X-APKD-Token", &settings.auth_token)
        .send()
        .map(|response| response.status().is_success())
        .unwrap_or(false)
}
