//! 設定の保持と保存。
//!
//! 追加した項目が古い設定ファイルに無くても読めるよう、全フィールドに
//! `#[serde(default)]` を効かせる。Portable 運用では利用者がファイルを持ち回るので、
//! バージョン違いで起動できなくなる方が事故として重い。

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::build_info;
use crate::paths;

/// APK の取得元。表示名と apkeep へ渡す値を一箇所に閉じ込める。
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize, Default)]
pub enum Source {
    /// 認証不要。まずこちらを既定にする。
    #[default]
    ApkPure,
    /// 利用者自身の Google アカウントで取得する。
    GooglePlay,
    FDroid,
}

impl Source {
    /// apkeep の `-d` に渡す値。
    pub fn cli_value(self) -> &'static str {
        match self {
            Self::ApkPure => "apk-pure",
            Self::GooglePlay => "google-play",
            Self::FDroid => "f-droid",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::ApkPure => "APKPure (認証不要)",
            Self::GooglePlay => "Google Play (自分のアカウント)",
            Self::FDroid => "F-Droid (OSS のみ)",
        }
    }

    pub const ALL: [Self; 3] = [Self::ApkPure, Self::GooglePlay, Self::FDroid];

    /// 資格情報が要るか。設定画面で入力欄を出すかの判断に使う。
    pub fn needs_credentials(self) -> bool {
        matches!(self, Self::GooglePlay)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize, Default)]
pub enum Theme {
    #[default]
    System,
    Light,
    Dark,
}

impl Theme {
    pub fn preference(self) -> egui::ThemePreference {
        match self {
            Self::System => egui::ThemePreference::System,
            Self::Light => egui::ThemePreference::Light,
            Self::Dark => egui::ThemePreference::Dark,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::System => "システムに合わせる",
            Self::Light => "ライト",
            Self::Dark => "ダーク",
        }
    }

    pub const ALL: [Self; 3] = [Self::System, Self::Light, Self::Dark];
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    // --- 取得 ---
    pub download_dir: PathBuf,
    pub source: Source,
    /// apkeep の実行ファイル。空なら PATH と既知の場所から自動で探す。
    pub apkeep_path: String,
    pub google_email: String,
    /// AAS トークン。パスワードそのものではないが、事実上の資格情報なので扱いは同等。
    pub google_aas_token: String,
    /// 取得完了後にダウンロードフォルダーを自動で開く。
    pub open_folder_on_complete: bool,

    // --- アプリ情報 ---
    /// 情報取得の基準となる国 (ISO 3166-1 alpha-2)。
    pub primary_region: String,
    /// 表示言語 (ISO 639-1)。
    pub language: String,
    /// 地域確認で横断的に見る国の一覧。
    pub regions: Vec<String>,

    // --- 連携 ---
    pub server_enabled: bool,
    pub server_port: u16,
    /// 拡張機能と共有する合言葉。localhost に開く以上、無認証にはしない。
    pub auth_token: String,
    pub clipboard_watch: bool,
    pub hotkey_enabled: bool,

    // --- 常駐 ---
    pub start_minimized: bool,
    pub close_to_tray: bool,
    pub notify_on_complete: bool,

    // --- 画面 ---
    pub theme: Theme,
    pub window_pos: Option<(f32, f32)>,
    pub window_size: Option<(f32, f32)>,
    pub info_panel_width: Option<f32>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            download_dir: paths::default_download_dir(),
            source: Source::default(),
            apkeep_path: String::new(),
            google_email: String::new(),
            google_aas_token: String::new(),
            open_folder_on_complete: false,

            primary_region: "JP".to_string(),
            language: "ja".to_string(),
            regions: default_regions(),

            server_enabled: true,
            server_port: build_info::DEFAULT_PORT,
            auth_token: generate_token(),
            clipboard_watch: false,
            hotkey_enabled: true,

            start_minimized: false,
            close_to_tray: true,
            notify_on_complete: true,

            theme: Theme::default(),
            window_pos: None,
            window_size: None,
            info_panel_width: None,
        }
    }
}

/// 地域確認の初期値。主要市場と、配信制限が出やすい国を混ぜてある。
pub fn default_regions() -> Vec<String> {
    ["JP", "US", "GB", "DE", "FR", "KR", "TW", "IN", "BR", "AU"]
        .iter()
        .map(|s| s.to_string())
        .collect()
}

impl Settings {
    pub fn path() -> PathBuf {
        paths::data_dir().join("settings.json")
    }

    pub fn load() -> Self {
        let path = Self::path();
        let Ok(text) = std::fs::read_to_string(&path) else {
            return Self::fresh();
        };
        match serde_json::from_str::<Self>(crate::paths::strip_bom(&text)) {
            Ok(mut settings) => {
                settings.repair();
                settings
            }
            Err(e) => {
                // 壊れた設定はそのまま上書きせず退避する。手で直したものを
                // 黙って消してしまうと、何を失ったかすら分からなくなる。
                let backup = path.with_extension("json.bak");
                let saved = std::fs::rename(&path, &backup).is_ok();
                crate::logfile::write(format!(
                    "設定を読めないので既定値で起動する: {e}{}",
                    if saved { format!(" (元の内容は {} へ退避)", backup.display()) } else { String::new() }
                ));
                Self::fresh()
            }
        }
    }

    /// 設定が無いときの既定値。**作ったその場で書き出す。**
    ///
    /// 合言葉は既定値を作るたびに引き直されるので、保存しないままだと、
    /// 設定を一度も変えずに落ちた (強制終了・シャットダウン・クラッシュ) 場合に
    /// 次の起動で別物になる。拡張へ貼り付けた合言葉が黙って合わなくなり、
    /// 右クリックしても 403 が返るだけで画面には何も起きない。原因が見えないので、
    /// 最初の一回で確定させておく。
    fn fresh() -> Self {
        let settings = Self::default();
        if let Err(e) = settings.save() {
            crate::logfile::write(format!("初回の設定を書き出せない: {e}"));
        }
        settings
    }

    pub fn save(&self) -> anyhow::Result<()> {
        let path = Self::path();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(&path, serde_json::to_string_pretty(self)?)?;
        Ok(())
    }

    /// 手で編集された設定でも起動できるところまで戻す。
    fn repair(&mut self) {
        if self.auth_token.trim().is_empty() {
            self.auth_token = generate_token();
        }
        if self.server_port == 0 {
            self.server_port = build_info::DEFAULT_PORT;
        }
        if self.download_dir.as_os_str().is_empty() {
            self.download_dir = paths::default_download_dir();
        }
        if self.primary_region.trim().is_empty() {
            self.primary_region = "JP".to_string();
        }
        if self.language.trim().is_empty() {
            self.language = "ja".to_string();
        }
        if self.regions.is_empty() {
            self.regions = default_regions();
        }
        // 国コードは大文字 2 文字に正規化する。小文字のまま送ると Play が無視する。
        for region in &mut self.regions {
            *region = region.trim().to_uppercase();
        }
        self.regions.retain(|r| r.len() == 2 && r.chars().all(|c| c.is_ascii_alphabetic()));
        self.primary_region = self.primary_region.trim().to_uppercase();
    }
}

/// 合言葉を作る。暗号用途ではないので、衝突しなければよい。
///
/// 乱数クレートを足すほどの要件ではないため、時刻とプロセス ID と
/// アドレスを混ぜて 32 桁の 16 進にする。
pub fn generate_token() -> String {
    use std::hash::{BuildHasher, Hasher, RandomState};

    let mut token = String::with_capacity(32);
    for salt in 0..4u64 {
        let mut hasher = RandomState::new().build_hasher();
        hasher.write_u64(salt);
        hasher.write_u128(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or_default(),
        );
        hasher.write_u32(std::process::id());
        hasher.write_usize(&token as *const String as usize);
        token.push_str(&format!("{:016x}", hasher.finish()));
    }
    token.truncate(32);
    token
}
