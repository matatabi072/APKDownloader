//! ビルドモードごとに変わる識別情報。
//!
//! debug と release を同時に起動でき、かつ見た目で即座に区別できるようにする。

#[cfg(debug_assertions)]
pub const WINDOW_TITLE: &str = "APK Downloader [DEBUG]";
#[cfg(not(debug_assertions))]
pub const WINDOW_TITLE: &str = "APK Downloader";

#[cfg(debug_assertions)]
pub const MUTEX_NAME: &str = r"Global\APKDownloader_SingleInstance_Debug";
#[cfg(not(debug_assertions))]
pub const MUTEX_NAME: &str = r"Global\APKDownloader_SingleInstance";

/// 設定ファイルの置き場。debug と release で混ざると検証が食い違うので分ける。
#[cfg(debug_assertions)]
pub const DATA_DIR_NAME: &str = "data-debug";
#[cfg(not(debug_assertions))]
pub const DATA_DIR_NAME: &str = "data";

/// 拡張機能からの受け口の既定ポート。debug と release が同時に起動できるよう別番号にする。
#[cfg(debug_assertions)]
pub const DEFAULT_PORT: u16 = 48916;
#[cfg(not(debug_assertions))]
pub const DEFAULT_PORT: u16 = 48915;

pub const IS_DEBUG: bool = cfg!(debug_assertions);

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
