//! アプリ本体の状態と、描画しない側の処理。
//!
//! eframe 0.35 では `App` が `logic()` (描画なし) と `ui()` (描画) に分かれている。
//! 常駐アプリではウィンドウを畳んでいる間も動き続ける必要があるので、
//! トレイの引き取り・ジョブの進行・通知はすべて `logic()` に置く。
//! `ui()` は `logic()` が整えた状態を映すだけにする。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use egui::ViewportCommand;
use global_hotkey::hotkey::{Code, HotKey, Modifiers};
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};

use crate::config::Settings;
use crate::history::History;
use crate::icons::IconCache;
use crate::jobs::{Engine, Event};
use crate::server::{self, ServerHandle};
use crate::tray::{self, Tray};
use crate::watcher::Watcher;
use crate::{build_info, logfile, notify, playstore, provider};

pub const DEFAULT_SIZE: [f32; 2] = [1020.0, 660.0];
pub const MIN_SIZE: [f32; 2] = [720.0, 460.0];

/// 取得の進み具合を見るための最小の再描画間隔。
const TICK: Duration = Duration::from_millis(250);

/// 中央の一覧で何を見ているか。
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Queue,
    History,
}

/// 取得ホットキー。Ctrl + Shift + D。
fn default_hotkey() -> HotKey {
    HotKey::new(Some(Modifiers::CONTROL | Modifiers::SHIFT), Code::KeyD)
}

pub struct App {
    pub settings: Settings,
    pub history: History,
    pub engine: Arc<Engine>,
    pub icons: IconCache,
    pub watcher: Watcher,

    tray: Option<Tray>,
    server: Option<ServerHandle>,
    hotkey: Option<HotkeyBinding>,
    /// まだ処理していないホットキーの押下数。専用スレッドが積む。
    hotkey_presses: Arc<AtomicUsize>,

    // --- 画面の状態 ---
    pub input: String,
    pub version_input: String,
    pub tab: Tab,
    pub selected_job: Option<u64>,
    pub show_log: bool,
    pub show_settings: bool,
    pub show_region: bool,
    /// 設定ウィンドウの編集中の値。取り消せるよう本体とは別に持つ。
    pub draft: Option<Settings>,
    pub draft_region_text: String,
    pub notice: Option<(String, bool)>,
    /// 右側の情報欄の幅。**初回だけ** Panel へ渡す。
    /// 毎フレーム渡すと egui が既定値へ戻すため (実測で幅が勝手に復帰する)。
    pub pending_panel_width: Option<f32>,

    show_main: bool,
    /// 未送出の表示切り替え要求。次のフレームで 1 回だけ命令にする。
    visibility_request: Option<bool>,
    allow_exit: bool,
    /// 設定の保存を間引くための印。ウィンドウ移動のたびに書くと I/O が無駄になる。
    settings_dirty: bool,
    last_settings_save: Instant,
    /// ウィンドウ位置の記録は落ち着いてから。起動直後は OS 側がまだ動かす。
    started_at: Instant,
}

struct HotkeyBinding {
    _manager: GlobalHotKeyManager,
    hotkey: HotKey,
    registered: bool,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, settings: Settings) -> Self {
        if crate::fonts::install_japanese(&cc.egui_ctx).is_none() {
            logfile::write("日本語フォントを読めなかった。文字化けする可能性がある");
        }
        cc.egui_ctx.set_theme(settings.theme.preference());
        // 情報カードの説明文などで行が詰まって見えるのを避ける。
        cc.egui_ctx.all_styles_mut(|style| {
            style.spacing.item_spacing = egui::vec2(8.0, 6.0);
        });

        let engine = Arc::new(Engine::new(cc.egui_ctx.clone(), &settings));
        let client = Arc::new(
            playstore::build_client().unwrap_or_else(|_| reqwest::blocking::Client::new()),
        );

        let tray = Tray::new(build_info::WINDOW_TITLE, cc.egui_ctx.clone(), || {
            // UI スレッドが眠っていても終われるようにする。トレイ監視スレッドから直接呼ばれる。
            logfile::write("トレイから終了要求");
        });
        if tray.is_none() {
            logfile::write("トレイアイコンを作れなかった。ウィンドウのみで動作する");
        }

        let watcher = Watcher::start(cc.egui_ctx.clone(), settings.clipboard_watch);
        let hotkey = install_hotkey(settings.hotkey_enabled);
        let hotkey_presses = start_hotkey_pump(&cc.egui_ctx);

        let mut app = Self {
            pending_panel_width: settings.info_panel_width,
            settings,
            history: History::load(),
            engine,
            icons: IconCache::new(client, cc.egui_ctx.clone()),
            watcher,
            tray,
            server: None,
            hotkey,
            hotkey_presses,

            input: String::new(),
            version_input: String::new(),
            tab: Tab::Queue,
            selected_job: None,
            show_log: false,
            show_settings: false,
            show_region: false,
            draft: None,
            draft_region_text: String::new(),
            notice: None,

            show_main: true,
            visibility_request: None,
            allow_exit: false,
            settings_dirty: false,
            last_settings_save: Instant::now(),
            started_at: Instant::now(),
        };

        app.show_main = !app.settings.start_minimized;
        app.sync_server();
        app
    }

    // ---------------------------------------------------------------- 操作

    /// 入力欄の内容で取得を始める。
    pub fn start_from_input(&mut self) {
        let input = self.input.trim().to_string();
        if input.is_empty() {
            self.set_notice("URL かパッケージ ID を入力してください", false);
            return;
        }
        let version = self.version_input.trim();
        let version = (!version.is_empty()).then(|| version.to_string());

        match self.engine.enqueue_input(&input, version) {
            Ok(id) => {
                self.selected_job = Some(id);
                self.tab = Tab::Queue;
                self.set_notice("取得を開始しました", true);
            }
            Err(message) => self.set_notice(message, false),
        }
    }

    /// 情報だけ見る。取得はしない。
    pub fn inspect_from_input(&mut self) {
        match playstore::parse_package_id(self.input.trim()) {
            Some(package) => {
                self.engine.inspect(&package);
                self.set_notice(format!("{package} の情報を取得しています"), true);
            }
            None => self.set_notice("URL かパッケージ ID として読めません", false),
        }
    }

    /// いま画面に出しているアプリのパッケージ ID。
    pub fn current_package(&self) -> Option<String> {
        if let Ok(state) = self.engine.inspect.lock()
            && !state.package.is_empty()
        {
            return Some(state.package.clone());
        }
        playstore::parse_package_id(self.input.trim())
    }

    pub fn open_download_dir(&mut self) {
        let dir = self.settings.download_dir.clone();
        if let Err(e) = std::fs::create_dir_all(&dir) {
            self.set_notice(format!("保存先を作れません: {e}"), false);
            return;
        }
        if let Err(e) = open::that(&dir) {
            self.set_notice(format!("フォルダーを開けません: {e}"), false);
        }
    }

    pub fn open_path(&mut self, path: &std::path::Path) {
        if let Err(e) = open::that(path) {
            self.set_notice(format!("開けません: {e}"), false);
        }
    }

    pub fn set_notice(&mut self, message: impl Into<String>, ok: bool) {
        self.notice = Some((message.into(), ok));
    }

    pub fn mark_settings_dirty(&mut self) {
        self.settings_dirty = true;
    }

    /// 設定ウィンドウで「適用」されたとき。副作用を持つ項目をここでまとめて反映する。
    pub fn apply_settings(&mut self, new_settings: Settings) {
        let theme_changed = new_settings.theme != self.settings.theme;
        self.settings = new_settings;
        self.settings.save().unwrap_or_else(|e| logfile::write(format!("設定を保存できない: {e}")));
        self.settings_dirty = false;

        self.engine.sync_settings(&self.settings);
        self.watcher.set_enabled(self.settings.clipboard_watch);
        self.sync_hotkey();
        self.sync_server();
        if theme_changed {
            // ここでは ctx を持っていないので、次の描画で反映させる。
            self.settings_dirty = true;
        }
    }

    fn sync_hotkey(&mut self) {
        let want = self.settings.hotkey_enabled;
        match (&mut self.hotkey, want) {
            (Some(binding), false) if binding.registered => {
                let _ = binding._manager.unregister(binding.hotkey);
                binding.registered = false;
            }
            (Some(binding), true) if !binding.registered => {
                binding.registered = binding._manager.register(binding.hotkey).is_ok();
            }
            (None, true) => self.hotkey = install_hotkey(true),
            _ => {}
        }
    }

    /// 受け口を設定に合わせる。ポートや合言葉が変わったら立て直す。
    fn sync_server(&mut self) {
        let want = self.settings.server_enabled;
        let matches = self
            .server
            .as_ref()
            .map(|s| s.matches(self.settings.server_port, &self.settings.auth_token))
            .unwrap_or(false);

        if !want {
            self.server = None;
            return;
        }
        if matches {
            return;
        }
        // 古い方を先に落とさないと、同じポートを掴めない。
        self.server = None;
        self.server = Some(server::start(
            self.settings.server_port,
            self.settings.auth_token.clone(),
            Arc::clone(&self.engine),
        ));
    }

    pub fn server_status(&self) -> server::Status {
        match &self.server {
            Some(handle) => handle.status(),
            None => server::Status::Stopped,
        }
    }

    pub fn apkeep_path(&self) -> Option<std::path::PathBuf> {
        provider::resolve(&self.settings.apkeep_path)
    }

    // ---------------------------------------------------------------- logic

    fn drain_finished_jobs(&mut self) {
        let entries = {
            let Ok(mut pending) = self.engine.pending_history.lock() else { return };
            std::mem::take(&mut *pending)
        };
        for entry in entries {
            self.history.push(entry);
        }
    }

    fn drain_events(&mut self) {
        let events = {
            let Ok(mut shared) = self.engine.shared.lock() else { return };
            std::mem::take(&mut shared.events)
        };

        for event in events {
            match event {
                Event::Finished { id, title, ok, detail } => {
                    if ok && self.settings.open_folder_on_complete {
                        self.open_download_dir();
                    }
                    // 失敗したら、そのジョブを選んで出力を開く。
                    // 出力欄は選択中のジョブにしか出ないので、拡張から投入したジョブが
                    // 失敗すると、原因が書かれた出力へ辿り着く手掛かりが画面に無かった。
                    if !ok {
                        self.selected_job = Some(id);
                        self.tab = Tab::Queue;
                        self.show_log = true;
                    }
                    if self.settings.notify_on_complete {
                        let heading = if ok { "取得しました" } else { "取得に失敗しました" };
                        notify::toast(&format!("{heading}: {title}"), &detail);
                    }
                    self.set_notice(format!("{title}: {detail}"), ok);
                }
            }
        }
    }

    fn poll_hotkey(&mut self) {
        for _ in 0..self.hotkey_presses.swap(0, Ordering::Relaxed) {
            match crate::watcher::read_clipboard().as_deref().and_then(playstore::parse_package_id)
            {
                Some(package) => {
                    logfile::write(format!("ホットキーから取得: {package}"));
                    self.engine.enqueue(&package, None);
                    self.tab = Tab::Queue;
                    self.set_notice(format!("{package} の取得を開始しました"), true);
                    notify::toast("取得を開始しました", &package);
                }
                None => {
                    notify::toast(
                        "取得できません",
                        "クリップボードに Google Play の URL がありません。",
                    );
                }
            }
        }
    }

    fn poll_tray(&mut self, ctx: &egui::Context) {
        let commands = self.tray.as_ref().map(Tray::poll).unwrap_or_default();
        for command in commands {
            match command {
                tray::Command::Show => self.set_main_visible(true),
                tray::Command::OpenFolder => self.open_download_dir(),
                tray::Command::OpenSettings => {
                    self.set_main_visible(true);
                    self.open_settings();
                }
                tray::Command::Exit => {
                    self.allow_exit = true;
                    ctx.send_viewport_cmd(ViewportCommand::Close);
                }
            }
        }
    }

    pub fn open_settings(&mut self) {
        self.draft = Some(self.settings.clone());
        self.draft_region_text = self.settings.regions.join(", ");
        self.show_settings = true;
    }

    fn update_tray(&mut self) {
        let (active, failed) = {
            let Ok(shared) = self.engine.shared.lock() else { return };
            (
                shared.active_count(),
                shared.jobs.iter().any(|j| j.phase == crate::jobs::Phase::Failed),
            )
        };

        let look = if active > 0 {
            tray::Look::Working
        } else if failed {
            tray::Look::Failed
        } else {
            tray::Look::Idle
        };
        let tooltip = if active > 0 {
            format!("{} — 取得中 {active} 件", build_info::WINDOW_TITLE)
        } else {
            build_info::WINDOW_TITLE.to_string()
        };

        if let Some(tray) = &mut self.tray {
            tray.update(look, &tooltip);
        }
    }

    /// 出す/隠すの要求が溜まっていれば流す。
    ///
    /// **毎フレーム `Visible(true)` を投げてはいけない。** 以前そうしていたところ、
    /// ✕ を押しても閉じない (トレイに格納されない) 状態になっていた。
    /// 要求があったときだけ 1 回送る。
    ///
    /// 現在値との差分ではなく「要求」で持つのは、外から `ShowWindow` で出される場合
    /// (別インスタンスの起動時など) があるため。差分で持つと自分の記憶と実際がずれ、
    /// 次の ✕ で「変化なし」と判断して隠れなくなる。
    fn sync_window(&mut self, ctx: &egui::Context) {
        if let Some(visible) = self.visibility_request.take() {
            logfile::write(format!("ウィンドウ表示を {visible} にする"));
            ctx.send_viewport_cmd(ViewportCommand::Visible(visible));
        }
    }

    /// ウィンドウを出す/隠す。表示状態を変えるときは必ずここを通す。
    fn set_main_visible(&mut self, visible: bool) {
        self.show_main = visible;
        self.visibility_request = Some(visible);
    }

    /// 二重起動した側からの「出して」を受ける。
    fn poll_show_request(&mut self, ctx: &egui::Context) {
        if self.engine.show_requested.swap(false, Ordering::Relaxed) {
            self.set_main_visible(true);
            ctx.send_viewport_cmd(ViewportCommand::Focus);
        }
    }

    fn remember_geometry(&mut self, ctx: &egui::Context) {
        // 起動直後は OS がまだウィンドウを動かす。落ち着くまで記録しない。
        if self.started_at.elapsed() < Duration::from_secs(2) || !self.show_main {
            return;
        }
        // 位置は外枠、大きさは**内側**で覚える。
        // 復元は `with_position` (外枠) と `with_inner_size` (内側) で行うので、
        // 揃えないと噛み合わない。外枠の大きさを内側として渡すと、再起動のたびに
        // 枠の分 (実測 16 x 39 px) だけウィンドウが膨らみ続ける。実際にそうなっていた。
        let (outer, inner) = ctx.input(|i| (i.viewport().outer_rect, i.viewport().inner_rect));
        let Some(outer) = outer else { return };
        let Some(inner) = inner else { return };

        let pos = (outer.min.x, outer.min.y);
        let size = (inner.width(), inner.height());
        let moved = self.settings.window_pos.map(|p| differs(p, pos)).unwrap_or(true);
        let resized = self.settings.window_size.map(|s| differs(s, size)).unwrap_or(true);

        if moved || resized {
            self.settings.window_pos = Some(pos);
            self.settings.window_size = Some(size);
            self.settings_dirty = true;
        }
    }

    fn save_settings_if_due(&mut self) {
        if !self.settings_dirty || self.last_settings_save.elapsed() < Duration::from_secs(3) {
            return;
        }
        self.settings_dirty = false;
        self.last_settings_save = Instant::now();
        if let Err(e) = self.settings.save() {
            logfile::write(format!("設定を保存できない: {e}"));
        }
    }
}

fn differs(a: (f32, f32), b: (f32, f32)) -> bool {
    (a.0 - b.0).abs() > 1.0 || (a.1 - b.1).abs() > 1.0
}

/// ホットキーの押下を拾う専用スレッドを立て、押された回数の置き場を返す。
///
/// **`logic()` の中で `try_recv` するだけでは足りない。** イベントループは入力が無いと
/// 眠っており、WM_HOTKEY は global-hotkey 自身の隠しウィンドウへ届くので、ウィンドウを
/// トレイへ格納している間は誰も `logic()` を回さない。実測 (2026-08) では、押しても何も
/// 起きず、**90 秒後**に別の用事でイベントループが目を覚ました瞬間にまとめて取得が
/// 始まった。押した本人はもう忘れている頃で、驚きにしかならない。
/// トレイ (`tray::spawn_event_pump`) と同じく、専用スレッドで受けて明示的に起こす。
///
/// 受け口はプロセスに 1 つしかないので、スレッドも 1 本だけ立てる。
fn start_hotkey_pump(ctx: &egui::Context) -> Arc<AtomicUsize> {
    static PRESSES: OnceLock<Arc<AtomicUsize>> = OnceLock::new();

    PRESSES
        .get_or_init(|| {
            let presses = Arc::new(AtomicUsize::new(0));
            let queue = Arc::clone(&presses);
            let ctx = ctx.clone();

            let started = std::thread::Builder::new()
                .name("hotkey-events".into())
                .spawn(move || {
                    while let Ok(event) = GlobalHotKeyEvent::receiver().recv() {
                        // 押した瞬間だけ拾う。離したときにも同じ id で飛んでくる。
                        if event.state != HotKeyState::Pressed {
                            continue;
                        }
                        queue.fetch_add(1, Ordering::Relaxed);
                        ctx.request_repaint_of(egui::ViewportId::ROOT);
                    }
                });
            if started.is_err() {
                logfile::write("ホットキー監視スレッドを立てられない。ホットキーは効かない");
            }
            presses
        })
        .clone()
}

/// ホットキーを登録する。奪われていたら諦めてログに残す。
///
/// Windows では登録したスレッドのメッセージループが要る。ここは必ずメインスレッド
/// (winit のイベントループ) から呼ぶこと。
fn install_hotkey(enabled: bool) -> Option<HotkeyBinding> {
    let manager = GlobalHotKeyManager::new()
        .inspect_err(|e| logfile::write(format!("ホットキーを扱えない: {e}")))
        .ok()?;
    let hotkey = default_hotkey();

    let registered = if enabled {
        match manager.register(hotkey) {
            Ok(()) => true,
            Err(e) => {
                logfile::write(format!("Ctrl+Shift+D を登録できない (他のアプリが使用中?): {e}"));
                false
            }
        }
    } else {
        false
    };

    Some(HotkeyBinding { _manager: manager, hotkey, registered })
}

impl App {
    /// ホットキーが実際に効いているか。設定画面に出す。
    pub fn hotkey_active(&self) -> bool {
        self.hotkey.as_ref().map(|h| h.registered).unwrap_or(false)
    }
}

impl eframe::App for App {
    /// 描画しない処理。ウィンドウが非表示でも呼ばれる。
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // egui はイベント駆動。取得中は明示的に起こさないと進捗が凍る。
        let active = self.engine.shared.lock().map(|s| s.active_count()).unwrap_or(0);
        let region_running = self.engine.region.lock().map(|r| r.running).unwrap_or(false);
        if active > 0 || region_running {
            ctx.request_repaint_after_for(TICK, egui::ViewportId::ROOT);
        }

        self.poll_tray(ctx);
        self.poll_show_request(ctx);
        self.poll_hotkey();
        self.drain_finished_jobs();
        self.drain_events();
        self.update_tray();

        // ✕ では終了せずトレイへ格納する。終了はトレイメニューから。
        if ctx.input(|i| i.viewport().close_requested()) {
            if self.allow_exit || !self.settings.close_to_tray || self.tray.is_none() {
                let _ = self.settings.save();
                logfile::write("終了する");
            } else {
                ctx.send_viewport_cmd(ViewportCommand::CancelClose);
                self.set_main_visible(false);
            }
        }

        self.sync_window(ctx);
        self.remember_geometry(ctx);
        self.save_settings_if_due();
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.icons.pump(ui.ctx());
        self.draw(ui);
    }
}
