//! 取得ジョブの管理。
//!
//! GUI スレッドは一切ブロックしない。通信・子プロセス起動はすべてここのワーカーが持ち、
//! 画面は `Arc<Mutex<..>>` 越しに状態を読むだけにする。
//!
//! ダウンロードは 1 本ずつ直列に流す。取得元 (APKPure / Play) はどちらも短時間に
//! 並行して叩くと弾かれるうえ、常駐アプリが回線を占有するのは趣旨に合わない。
//! 一方、情報取得と地域確認はダウンロードを待たせたくないので別スレッドで走らせる。

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};

use chrono::{DateTime, Local};

use crate::config::{Settings, Source};
use crate::history::Entry;
use crate::playstore::{self, AppInfo};
use crate::provider::{self, Credentials};
use crate::region::{self, RegionReport};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Phase {
    Queued,
    /// ストアページからアプリ情報を取っている。
    Resolving,
    Downloading,
    Done,
    Failed,
    Canceled,
}

impl Phase {
    pub fn label(self) -> &'static str {
        match self {
            Self::Queued => "待機中",
            Self::Resolving => "情報取得中",
            Self::Downloading => "取得中",
            Self::Done => "完了",
            Self::Failed => "失敗",
            Self::Canceled => "中止",
        }
    }

    pub fn is_active(self) -> bool {
        matches!(self, Self::Queued | Self::Resolving | Self::Downloading)
    }

    pub fn color(self) -> egui::Color32 {
        match self {
            Self::Queued => egui::Color32::from_rgb(140, 145, 155),
            Self::Resolving | Self::Downloading => egui::Color32::from_rgb(70, 150, 210),
            Self::Done => egui::Color32::from_rgb(60, 170, 90),
            Self::Failed => egui::Color32::from_rgb(200, 80, 70),
            Self::Canceled => egui::Color32::from_rgb(170, 140, 60),
        }
    }
}

pub struct Job {
    pub id: u64,
    pub package: String,
    pub requested_version: Option<String>,
    pub source: Source,
    pub phase: Phase,
    pub info: Option<AppInfo>,
    /// 直近の 1 行。一覧に出す。
    pub message: String,
    /// 子プロセスの出力全体。詳細を開いたときに見せる。
    pub log: Vec<String>,
    pub files: Vec<PathBuf>,
    pub total_bytes: u64,
    pub created_at: DateTime<Local>,
    pub finished_at: Option<DateTime<Local>>,
    cancel: Arc<AtomicBool>,
}

impl Job {
    pub fn title(&self) -> &str {
        self.info.as_ref().and_then(|i| i.name.as_deref()).unwrap_or(&self.package)
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    pub fn is_canceling(&self) -> bool {
        self.phase.is_active() && self.cancel.load(Ordering::Relaxed)
    }
}

/// 画面へ知らせたい出来事。通知の発火に使う。
#[derive(Clone, Debug)]
pub enum Event {
    Finished { id: u64, title: String, ok: bool, detail: String },
}

#[derive(Default)]
pub struct Shared {
    pub jobs: Vec<Job>,
    pub events: Vec<Event>,
    next_id: u64,
}

impl Shared {
    pub fn job(&self, id: u64) -> Option<&Job> {
        self.jobs.iter().find(|j| j.id == id)
    }

    fn job_mut(&mut self, id: u64) -> Option<&mut Job> {
        self.jobs.iter_mut().find(|j| j.id == id)
    }

    pub fn active_count(&self) -> usize {
        self.jobs.iter().filter(|j| j.phase.is_active()).count()
    }
}

/// ワーカーが参照する設定。画面側で変更したら `Engine::sync_settings` で入れ替える。
#[derive(Clone)]
pub struct EngineSettings {
    pub download_dir: PathBuf,
    pub source: Source,
    pub apkeep_path: String,
    pub credentials: Credentials,
    pub language: String,
    pub region: String,
    pub regions: Vec<String>,
}

impl EngineSettings {
    pub fn from(settings: &Settings) -> Self {
        Self {
            download_dir: settings.download_dir.clone(),
            source: settings.source,
            apkeep_path: settings.apkeep_path.clone(),
            credentials: Credentials {
                email: settings.google_email.clone(),
                aas_token: settings.google_aas_token.clone(),
            },
            language: settings.language.clone(),
            region: settings.primary_region.clone(),
            regions: settings.regions.clone(),
        }
    }
}

/// アプリ情報だけを見に行った結果。
#[derive(Default)]
pub struct InspectState {
    pub package: String,
    pub running: bool,
    pub info: Option<AppInfo>,
    pub error: Option<String>,
    /// 取得できるバージョン一覧。要求されたときだけ埋まる。
    pub versions: Option<Vec<String>>,
    pub versions_error: Option<String>,
    pub versions_running: bool,
}

/// 地域確認の進行状況。
pub struct RegionState {
    pub package: String,
    pub running: bool,
    pub done: usize,
    pub total: usize,
    pub report: Option<RegionReport>,
    pub error: Option<String>,
    cancel: Arc<AtomicBool>,
}

impl Default for RegionState {
    fn default() -> Self {
        Self {
            package: String::new(),
            running: false,
            done: 0,
            total: 0,
            report: None,
            error: None,
            cancel: Arc::new(AtomicBool::new(false)),
        }
    }
}

impl RegionState {
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

/// 完了したジョブから履歴へ積むための受け渡し。
/// 履歴の所有は画面側 (App) に置き、ワーカーはここへ置くだけにする。
pub type PendingHistory = Arc<Mutex<Vec<Entry>>>;

pub struct Engine {
    pub shared: Arc<Mutex<Shared>>,
    pub inspect: Arc<Mutex<InspectState>>,
    pub region: Arc<Mutex<RegionState>>,
    pub pending_history: PendingHistory,
    /// 「ウィンドウを出してほしい」という外からの要求。
    ///
    /// トレイ格納中に EXE をもう一度起動したときに立つ。二重起動した側が
    /// `ShowWindow` で直接出すと winit の内部状態とずれ、そのあと ✕ を押しても
    /// 隠れなくなる (実測)。表示の操作は必ずこのアプリ自身にやらせる。
    pub show_requested: Arc<AtomicBool>,
    settings: Arc<Mutex<EngineSettings>>,
    tx: mpsc::Sender<u64>,
    ctx: egui::Context,
    client: Arc<reqwest::blocking::Client>,
}

impl Engine {
    pub fn new(ctx: egui::Context, settings: &Settings) -> Self {
        let client = Arc::new(playstore::build_client().unwrap_or_else(|e| {
            crate::logfile::write(format!("HTTP クライアントを作れないので既定値を使う: {e}"));
            reqwest::blocking::Client::new()
        }));

        let (tx, rx) = mpsc::channel::<u64>();
        let engine = Self {
            shared: Arc::new(Mutex::new(Shared::default())),
            inspect: Arc::new(Mutex::new(InspectState::default())),
            region: Arc::new(Mutex::new(RegionState::default())),
            pending_history: Arc::new(Mutex::new(Vec::new())),
            show_requested: Arc::new(AtomicBool::new(false)),
            settings: Arc::new(Mutex::new(EngineSettings::from(settings))),
            tx,
            ctx,
            client,
        };

        engine.spawn_worker(rx);
        engine
    }

    pub fn sync_settings(&self, settings: &Settings) {
        if let Ok(mut current) = self.settings.lock() {
            *current = EngineSettings::from(settings);
        }
    }

    fn settings_snapshot(&self) -> EngineSettings {
        self.settings.lock().expect("engine settings poisoned").clone()
    }

    /// 画面を起こす。ワーカーが状態を書き換えたら必ず呼ぶ。
    /// これが無いと、egui はイベント駆動なので進捗が止まって見える。
    pub fn wake(&self) {
        self.ctx.request_repaint();
    }

    /// URL でも パッケージ ID でも受け取り、取得を始める。
    ///
    /// 拡張機能・ホットキー・手入力の入口をここに集約している。
    pub fn enqueue_input(&self, input: &str, version: Option<String>) -> Result<u64, String> {
        let package = playstore::parse_package_id(input).ok_or_else(|| {
            format!("Google Play の URL かパッケージ ID として読めません: {input}")
        })?;
        Ok(self.enqueue(&package, version))
    }

    pub fn enqueue(&self, package: &str, version: Option<String>) -> u64 {
        let source = self.settings_snapshot().source;
        let id = {
            let mut shared = self.shared.lock().expect("jobs poisoned");
            shared.next_id += 1;
            let id = shared.next_id;
            shared.jobs.insert(
                0,
                Job {
                    id,
                    package: package.to_string(),
                    requested_version: version,
                    source,
                    phase: Phase::Queued,
                    info: None,
                    message: "順番待ち".to_string(),
                    log: Vec::new(),
                    files: Vec::new(),
                    total_bytes: 0,
                    created_at: Local::now(),
                    finished_at: None,
                    cancel: Arc::new(AtomicBool::new(false)),
                },
            );
            id
        };

        crate::logfile::write(format!("ジョブ #{id} を登録: {package}"));
        if self.tx.send(id).is_err() {
            self.fail(id, "ワーカーが停止しています".to_string());
        }
        self.wake();
        // 画面に出すため、待っている間に情報だけ先に取っておく。
        self.inspect(package);
        id
    }

    /// アプリ情報だけを取りに行く。取得はしない。
    pub fn inspect(&self, package: &str) {
        {
            let mut state = self.inspect.lock().expect("inspect poisoned");
            // 同じアプリを見ている最中なら二重に投げない。
            if state.running && state.package == package {
                return;
            }
            *state = InspectState {
                package: package.to_string(),
                running: true,
                ..Default::default()
            };
        }

        let package = package.to_string();
        let client = Arc::clone(&self.client);
        let state = Arc::clone(&self.inspect);
        let settings = self.settings_snapshot();
        let ctx = self.ctx.clone();

        std::thread::spawn(move || {
            let result =
                playstore::fetch_info(&client, &package, &settings.language, &settings.region);

            if let Ok(mut state) = state.lock() {
                // 待っている間に別のアプリへ切り替わっていたら、古い結果は捨てる。
                if state.package == package {
                    state.running = false;
                    match result {
                        Ok(info) => state.info = Some(info),
                        Err(e) => state.error = Some(e.to_string()),
                    }
                }
            }
            ctx.request_repaint();
        });
    }

    /// 取得できるバージョンの一覧を調べる。apkeep に問い合わせる。
    pub fn fetch_versions(&self, package: &str) {
        {
            let mut state = self.inspect.lock().expect("inspect poisoned");
            if state.versions_running {
                return;
            }
            state.versions_running = true;
            state.versions_error = None;
        }

        let package = package.to_string();
        let state = Arc::clone(&self.inspect);
        let settings = self.settings_snapshot();
        let ctx = self.ctx.clone();

        std::thread::spawn(move || {
            let result = provider::prepare(
                &settings.apkeep_path,
                settings.source,
                &settings.credentials,
            )
            .and_then(|backend| {
                let cancel = Arc::new(AtomicBool::new(false));
                provider::list_versions(&backend, &package, &cancel).map_err(|e| e.to_string())
            });

            if let Ok(mut state) = state.lock() {
                state.versions_running = false;
                if state.package == package {
                    match result {
                        Ok(versions) => state.versions = Some(versions),
                        Err(message) => state.versions_error = Some(message),
                    }
                }
            }
            ctx.request_repaint();
        });
    }

    /// 地域ごとの配信状況を調べる。
    pub fn check_regions(&self, package: &str) {
        let cancel = Arc::new(AtomicBool::new(false));
        {
            let mut state = self.region.lock().expect("region poisoned");
            if state.running {
                return;
            }
            *state = RegionState {
                package: package.to_string(),
                running: true,
                cancel: Arc::clone(&cancel),
                ..Default::default()
            };
        }

        let package = package.to_string();
        let client = Arc::clone(&self.client);
        let state = Arc::clone(&self.region);
        let settings = self.settings_snapshot();
        let ctx = self.ctx.clone();

        std::thread::spawn(move || {
            let progress_state = Arc::clone(&state);
            let progress_ctx = ctx.clone();
            let report = region::check(
                &client,
                &package,
                &settings.language,
                &settings.regions,
                &cancel,
                move |done, total| {
                    if let Ok(mut state) = progress_state.lock() {
                        state.done = done;
                        state.total = total;
                    }
                    progress_ctx.request_repaint();
                },
            );

            if let Ok(mut state) = state.lock() {
                state.running = false;
                if cancel.load(Ordering::Relaxed) {
                    state.error = Some("中止しました".to_string());
                } else {
                    state.report = Some(report);
                }
            }
            ctx.request_repaint();
        });
    }

    fn fail(&self, id: u64, message: String) {
        let mut shared = self.shared.lock().expect("jobs poisoned");
        if let Some(job) = shared.job_mut(id) {
            job.phase = Phase::Failed;
            job.message = message.clone();
            job.log.push(message);
            job.finished_at = Some(Local::now());
        }
    }

    /// 取得を実際に走らせるスレッド。1 本だけ立てて直列に処理する。
    fn spawn_worker(&self, rx: mpsc::Receiver<u64>) {
        let shared = Arc::clone(&self.shared);
        let settings = Arc::clone(&self.settings);
        let pending_history = Arc::clone(&self.pending_history);
        let client = Arc::clone(&self.client);
        let ctx = self.ctx.clone();

        std::thread::Builder::new()
            .name("apk-worker".into())
            .spawn(move || {
                for id in rx {
                    run_job(id, &shared, &settings, &pending_history, &client, &ctx);
                    ctx.request_repaint();
                }
            })
            .expect("ワーカーの起動に失敗");
    }
}

/// 1 件分の取得。情報取得 → ダウンロード → 履歴の順に進める。
fn run_job(
    id: u64,
    shared: &Arc<Mutex<Shared>>,
    settings: &Arc<Mutex<EngineSettings>>,
    pending_history: &PendingHistory,
    client: &reqwest::blocking::Client,
    ctx: &egui::Context,
) {
    let settings = settings.lock().expect("engine settings poisoned").clone();

    // ジョブの素性と中止フラグを取り出す。以後ロックは短く保つ。
    let Some((package, version, cancel)) = ({
        let mut guard = shared.lock().expect("jobs poisoned");
        guard.job_mut(id).map(|job| {
            job.phase = Phase::Resolving;
            job.message = "アプリ情報を取得中".to_string();
            (job.package.clone(), job.requested_version.clone(), Arc::clone(&job.cancel))
        })
    }) else {
        return;
    };
    ctx.request_repaint();

    if cancel.load(Ordering::Relaxed) {
        finish(shared, id, Phase::Canceled, "中止しました".to_string(), ctx);
        return;
    }

    // 情報は取れなくても取得は続ける。ミラー側にあれば APK は落ちるため、
    // ここで失敗にすると「取れるはずのものが取れない」ことになる。
    let info = match playstore::fetch_info(client, &package, &settings.language, &settings.region) {
        Ok(info) => Some(info),
        Err(e) => {
            push_log(shared, id, format!("アプリ情報を取得できず続行します: {e}"));
            None
        }
    };
    if let Ok(mut guard) = shared.lock()
        && let Some(job) = guard.job_mut(id)
    {
        job.info = info.clone();
    }
    ctx.request_repaint();

    // 取得元の準備を確認する。
    let backend =
        match provider::prepare(&settings.apkeep_path, settings.source, &settings.credentials) {
            Ok(backend) => backend,
            Err(message) => {
                finish(shared, id, Phase::Failed, message.clone(), ctx);
                emit(shared, Event::Finished { id, title: package, ok: false, detail: message });
                return;
            }
        };

    set_phase(shared, id, Phase::Downloading, "取得を開始".to_string());
    ctx.request_repaint();

    let outcome = provider::download(
        &backend,
        &package,
        version.as_deref(),
        &settings.download_dir,
        &cancel,
        |line| {
            push_log(shared, id, line);
            ctx.request_repaint();
        },
    );

    let title = info
        .as_ref()
        .and_then(|i| i.name.clone())
        .unwrap_or_else(|| package.clone());

    match outcome {
        Ok(result) => {
            let names: Vec<String> = result
                .files
                .iter()
                .filter_map(|p| p.file_name())
                .map(|n| n.to_string_lossy().into_owned())
                .collect();
            let detail = names.join(", ");

            if let Ok(mut guard) = shared.lock()
                && let Some(job) = guard.job_mut(id)
            {
                job.files = result.files.clone();
                job.total_bytes = result.total_bytes;
            }

            if let Ok(mut queue) = pending_history.lock() {
                queue.push(Entry {
                    package: package.clone(),
                    name: info.as_ref().and_then(|i| i.name.clone()).unwrap_or_default(),
                    developer: info.as_ref().and_then(|i| i.developer.clone()).unwrap_or_default(),
                    version: info
                        .as_ref()
                        .and_then(|i| i.version.clone())
                        .or(version.clone())
                        .unwrap_or_default(),
                    icon_url: info.as_ref().and_then(|i| i.icon_url.clone()).unwrap_or_default(),
                    source: settings.source,
                    finished_at: Local::now(),
                    files: result.files,
                    total_bytes: result.total_bytes,
                });
            }

            finish(shared, id, Phase::Done, detail.clone(), ctx);
            emit(shared, Event::Finished { id, title, ok: true, detail });
        }
        Err(e) => {
            let canceled = cancel.load(Ordering::Relaxed);
            let phase = if canceled { Phase::Canceled } else { Phase::Failed };
            let message = e.to_string();
            finish(shared, id, phase, message.clone(), ctx);
            if !canceled {
                emit(shared, Event::Finished { id, title, ok: false, detail: message });
            }
        }
    }
}

fn set_phase(shared: &Arc<Mutex<Shared>>, id: u64, phase: Phase, message: String) {
    if let Ok(mut guard) = shared.lock()
        && let Some(job) = guard.job_mut(id)
    {
        job.phase = phase;
        job.message = message;
    }
}

fn push_log(shared: &Arc<Mutex<Shared>>, id: u64, line: String) {
    if let Ok(mut guard) = shared.lock()
        && let Some(job) = guard.job_mut(id)
    {
        job.message = line.clone();
        job.log.push(line);
        // 出力が多いツールでも青天井にしない。
        if job.log.len() > 400 {
            job.log.drain(..100);
        }
    }
}

fn finish(
    shared: &Arc<Mutex<Shared>>,
    id: u64,
    phase: Phase,
    message: String,
    ctx: &egui::Context,
) {
    if let Ok(mut guard) = shared.lock()
        && let Some(job) = guard.job_mut(id)
    {
        job.phase = phase;
        job.message = message.clone();
        job.log.push(format!("[{}] {message}", phase.label()));
        job.finished_at = Some(Local::now());
    }
    crate::logfile::write(format!("ジョブ #{id} {}: {message}", phase.label()));
    ctx.request_repaint();
}

fn emit(shared: &Arc<Mutex<Shared>>, event: Event) {
    if let Ok(mut guard) = shared.lock() {
        guard.events.push(event);
    }
}
