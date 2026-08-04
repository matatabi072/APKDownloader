//! 画面の描画。
//!
//! 状態を持つ処理は `app.rs` 側に置き、ここは映すことと入力を拾うことに徹する。
//!
//! 共有状態 (ジョブ・情報) を握ったまま `&mut self` のメソッドを呼ぶと、その場で
//! デッドロックするか借用検査に落ちる。**ロックは必ず「複製して即座に離す」**
//! 方針で扱い、押されたボタンは列挙型に溜めて描画後にまとめて処理する。

use std::path::PathBuf;

use egui::{Align, Color32, Id, Layout, RichText};

use crate::app::{App, Tab};
use crate::config::Source;
use crate::jobs::Phase;
use crate::{build_info, provider};

/// 補助テキストの色。
///
/// `.weak()` と `hint_text(&str)` はダークテーマで背景に溶けて読めなくなる
/// (どちらも `weak_text_color()` を使う)。本文色と背景色を 82:18 で混ぜて、
/// 控えめだが確実に読める中間トーンを作る。
pub fn hint(ui: &egui::Ui, text: &str) -> RichText {
    let base = ui.visuals().text_color();
    let bg = ui.visuals().extreme_bg_color;
    let blend = |t: u8, b: u8| ((t as u16 * 82 + b as u16 * 18) / 100) as u8;
    RichText::new(text).color(Color32::from_rgb(
        blend(base.r(), bg.r()),
        blend(base.g(), bg.g()),
        blend(base.b(), bg.b()),
    ))
}

/// 一覧の行から出てくる操作。描画中に self を触らないための伝票。
enum JobAction {
    Select(u64),
    Cancel(u64),
    OpenFolder(PathBuf),
    OpenFile(PathBuf),
    Retry(String),
}

enum HistoryAction {
    OpenFile(PathBuf),
    OpenFolder(PathBuf),
    Retry(String),
    Inspect(String),
    Remove(usize),
}

/// 描画に必要な分だけ写し取ったジョブ。
struct JobRow {
    id: u64,
    title: String,
    package: String,
    phase: Phase,
    message: String,
    icon_url: String,
    version: Option<String>,
    /// 登録した時点の取得元。後から設定を変えても、この行が何で取ったかは変わらない。
    source: Source,
    files: Vec<PathBuf>,
    canceling: bool,
    created: String,
}

impl App {
    pub fn draw(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();

        egui::Panel::top(Id::new("apkd_top")).show(ui, |ui| self.top_bar(ui));
        egui::Panel::bottom(Id::new("apkd_status")).show(ui, |ui| self.status_bar(ui));
        self.job_log_panel(ui);

        let mut panel = egui::Panel::right(Id::new("apkd_info"))
            .resizable(true)
            .size_range(260.0..=560.0);
        // **初回だけ**幅を渡す。毎フレーム渡すと egui が「既定へ戻せ」と解釈して、
        // 利用者が縮めた幅が勝手に復帰する。
        if let Some(width) = self.pending_panel_width.take() {
            panel = panel.default_size(width);
        }
        let panel_response = panel.show(ui, |ui| self.info_panel(ui));

        let width = panel_response.response.rect.width();
        if (self.settings.info_panel_width.unwrap_or(-1.0) - width).abs() > 0.5 {
            self.settings.info_panel_width = Some(width);
            self.mark_settings_dirty();
        }

        egui::Frame::central_panel(ui.style()).show(ui, |ui| self.center(ui));

        self.settings_window(&ctx);
        self.region_window(&ctx);
    }

    // ------------------------------------------------------------- 上部

    fn top_bar(&mut self, ui: &mut egui::Ui) {
        ui.add_space(4.0);

        ui.horizontal(|ui| {
            ui.label(RichText::new("URL / パッケージ ID").strong());

            // 右端のボタンを先に置いてから、残りを入力欄に充てる。
            let buttons_width = 210.0;
            let field_width = (ui.available_width() - buttons_width).max(180.0);

            let field = ui.add_sized(
                [field_width, 24.0],
                egui::TextEdit::singleline(&mut self.input)
                    .hint_text(hint(ui, "https://play.google.com/store/apps/details?id=…")),
            );
            // Enter で取得を始められるようにする。都度ボタンへ手を伸ばさせない。
            if field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                self.start_from_input();
            }

            if ui.button("取得").on_hover_text("APK のダウンロードを開始 (Enter)").clicked() {
                self.start_from_input();
            }
            if ui.button("情報のみ").on_hover_text("ダウンロードせずアプリ情報だけ見る").clicked() {
                self.inspect_from_input();
            }
            if ui.button("貼り付け").clicked() {
                match crate::watcher::read_clipboard() {
                    Some(text) => {
                        self.input = text;
                        self.inspect_from_input();
                    }
                    None => self.set_notice("クリップボードに文字列がありません", false),
                }
            }
        });

        ui.horizontal(|ui| {
            ui.label("取得元");
            let mut source = self.settings.source;
            egui::ComboBox::from_id_salt("apkd_source")
                .selected_text(source.label())
                .width(220.0)
                .show_ui(ui, |ui| {
                    for candidate in crate::config::Source::ALL {
                        ui.selectable_value(&mut source, candidate, candidate.label());
                    }
                });
            if source != self.settings.source {
                self.settings.source = source;
                self.engine.sync_settings(&self.settings);
                self.mark_settings_dirty();
            }

            ui.separator();
            ui.label("バージョン");
            ui.add(
                egui::TextEdit::singleline(&mut self.version_input)
                    .desired_width(110.0)
                    .hint_text(hint(ui, "最新")),
            )
            .on_hover_text("空欄なら最新版。取得元にある版だけ指定できます。");

            if ui.button("一覧").clicked() {
                match self.current_package() {
                    Some(package) => self.engine.fetch_versions(&package),
                    None => self.set_notice("先に URL かパッケージ ID を入れてください", false),
                }
            }

            ui.separator();
            if ui.button("地域を確認").clicked() {
                match self.current_package() {
                    Some(package) => {
                        self.engine.check_regions(&package);
                        self.show_region = true;
                    }
                    None => self.set_notice("先に URL かパッケージ ID を入れてください", false),
                }
            }

            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.button("設定").clicked() {
                    self.open_settings();
                }
                if ui.button("保存フォルダー").clicked() {
                    self.open_download_dir();
                }
            });
        });

        self.version_list_row(ui);
        self.clipboard_banner(ui);
        ui.add_space(4.0);
    }

    /// `一覧` で取れたバージョンを並べる。押すとバージョン欄に入る。
    fn version_list_row(&mut self, ui: &mut egui::Ui) {
        let (running, versions, error) = {
            let Ok(state) = self.engine.inspect.lock() else { return };
            (state.versions_running, state.versions.clone(), state.versions_error.clone())
        };

        if running {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(hint(ui, "取得できるバージョンを調べています…"));
            });
            return;
        }
        if let Some(message) = error {
            ui.label(RichText::new(format!("バージョン一覧: {message}")).color(Color32::from_rgb(200, 90, 80)));
            return;
        }
        let Some(versions) = versions else { return };
        if versions.is_empty() {
            ui.label(hint(ui, "取得元からバージョン一覧を得られませんでした"));
            return;
        }

        ui.horizontal_wrapped(|ui| {
            ui.label(hint(ui, "取得できる版:"));
            // 多すぎると一行が埋まるので上位だけ出す。全部は apkeep の出力に任せる。
            for version in versions.iter().take(24) {
                if ui.small_button(version).clicked() {
                    self.version_input = version.clone();
                }
            }
            if versions.len() > 24 {
                ui.label(hint(ui, &format!("ほか {} 件", versions.len() - 24)));
            }
        });
    }

    /// クリップボードで見つけた候補。**自動では始めない。**
    fn clipboard_banner(&mut self, ui: &mut egui::Ui) {
        let Some((package, source_text)) = self.watcher.peek() else { return };

        egui::Frame::group(ui.style())
            .fill(ui.visuals().faint_bg_color)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("コピーされた URL を見つけました").strong());
                    ui.label(hint(ui, &elide(&source_text, 60)));

                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.button("閉じる").clicked() {
                            self.watcher.dismiss();
                        }
                        if ui.button("情報を見る").clicked() {
                            self.input = package.clone();
                            self.engine.inspect(&package);
                            self.watcher.clear();
                        }
                        if ui.button("これを取得").clicked() {
                            self.input = package.clone();
                            self.engine.enqueue(&package, None);
                            self.tab = Tab::Queue;
                            self.watcher.clear();
                        }
                    });
                });
            });
    }

    // ------------------------------------------------------------- 下部

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        ui.add_space(2.0);
        ui.horizontal(|ui| {
            if build_info::IS_DEBUG {
                ui.label(
                    RichText::new("DEBUG")
                        .color(Color32::from_rgb(230, 180, 40))
                        .strong()
                        .monospace(),
                );
                ui.separator();
            }

            let status = self.server_status();
            let color = if status.is_listening() {
                Color32::from_rgb(60, 170, 90)
            } else {
                Color32::from_rgb(200, 130, 60)
            };
            ui.label("受け口");
            ui.label(RichText::new(status.label()).color(color));

            ui.separator();
            ui.label("apkeep");
            match self.apkeep_path() {
                Some(path) => {
                    ui.label(RichText::new("あり").color(Color32::from_rgb(60, 170, 90)))
                        .on_hover_text(path.display().to_string());
                }
                None => {
                    ui.label(RichText::new("未検出").color(Color32::from_rgb(200, 90, 80)))
                        .on_hover_text(provider::SETUP_HINT);
                }
            }

            ui.separator();

            // 右詰めの通知を**先に**置き、余った幅で保存先を出す。
            // 左から順に置くと、どちらも長いときに互いの上へ描かれて重なる。
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if let Some((message, ok)) = &self.notice {
                    let color = if *ok {
                        Color32::from_rgb(60, 170, 90)
                    } else {
                        Color32::from_rgb(200, 90, 80)
                    };
                    // 通知が幅を独占しないよう字数で抑える。全文はホバーで読める。
                    ui.add(
                        egui::Label::new(RichText::new(elide(message, 60)).color(color))
                            .truncate(),
                    )
                    .on_hover_text(message.clone());
                }

                // 残りは左詰めに戻す。入りきらないパスは省略記号で畳む。
                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                    let path = format!("保存先: {}", self.settings.download_dir.display());
                    ui.add(egui::Label::new(hint(ui, &path)).truncate())
                        .on_hover_text(path);
                });
            });
        });
        ui.add_space(2.0);
    }

    // ------------------------------------------------------------- 中央

    fn center(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let active = self.engine.shared.lock().map(|s| s.active_count()).unwrap_or(0);
            let queue_label =
                if active > 0 { format!("取得キュー ({active})") } else { "取得キュー".to_string() };
            ui.selectable_value(&mut self.tab, Tab::Queue, queue_label);
            ui.selectable_value(
                &mut self.tab,
                Tab::History,
                format!("履歴 ({})", self.history.entries().len()),
            );

            ui.with_layout(Layout::right_to_left(Align::Center), |ui| match self.tab {
                Tab::Queue => {
                    if ui.button("完了分を消す").clicked()
                        && let Ok(mut shared) = self.engine.shared.lock()
                    {
                        shared.jobs.retain(|job| job.phase.is_active());
                    }
                }
                Tab::History => {
                    if ui.button("履歴を全消去").clicked() {
                        self.history.clear();
                    }
                }
            });
        });
        ui.separator();

        match self.tab {
            Tab::Queue => self.queue_list(ui),
            Tab::History => self.history_list(ui),
        }
    }

    fn queue_list(&mut self, ui: &mut egui::Ui) {
        // ロックは写し取るまで。以降は self を自由に触れる。
        let rows: Vec<JobRow> = {
            let Ok(shared) = self.engine.shared.lock() else { return };
            shared
                .jobs
                .iter()
                .map(|job| JobRow {
                    id: job.id,
                    title: job.title().to_string(),
                    package: job.package.clone(),
                    phase: job.phase,
                    message: job.message.clone(),
                    icon_url: job
                        .info
                        .as_ref()
                        .and_then(|i| i.icon_url.clone())
                        .unwrap_or_default(),
                    version: job
                        .info
                        .as_ref()
                        .and_then(|i| i.version.clone())
                        .or_else(|| job.requested_version.clone()),
                    source: job.source,
                    files: job.files.clone(),
                    canceling: job.is_canceling(),
                    created: job.created_at.format("%H:%M:%S").to_string(),
                })
                .collect()
        };

        if rows.is_empty() {
            ui.add_space(24.0);
            ui.vertical_centered(|ui| {
                ui.label(hint(ui, "取得はまだありません。"));
                ui.label(hint(
                    ui,
                    "上の欄に Google Play の URL を貼るか、ブラウザーの右クリックから送ってください。",
                ));
            });
            return;
        }

        let mut action = None;
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            for row in &rows {
                let selected = self.selected_job == Some(row.id);
                let response = egui::Frame::group(ui.style())
                    .fill(if selected {
                        ui.visuals().selection.bg_fill.gamma_multiply(0.25)
                    } else {
                        ui.visuals().faint_bg_color
                    })
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            if let Some(texture) = self.icons.get(&row.icon_url) {
                                ui.add(egui::Image::new(&texture).fit_to_exact_size(egui::vec2(40.0, 40.0)));
                            } else {
                                ui.allocate_space(egui::vec2(40.0, 40.0));
                            }

                            ui.vertical(|ui| {
                                ui.horizontal(|ui| {
                                    ui.label(RichText::new(&row.title).strong());
                                    if let Some(version) = &row.version {
                                        ui.label(hint(ui, &format!("v{version}")));
                                    }
                                    ui.label(
                                        RichText::new(row.phase.label())
                                            .color(row.phase.color())
                                            .small(),
                                    );
                                    if row.canceling {
                                        ui.label(hint(ui, "(中止しています…)"));
                                    }
                                });
                                ui.horizontal(|ui| {
                                    ui.label(hint(ui, &row.package));
                                    ui.label(hint(ui, "・"));
                                    ui.label(hint(ui, row.source.label()));
                                });
                                ui.label(hint(ui, &elide(&row.message, 110)))
                                    .on_hover_text(&row.message);
                            });

                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if row.phase.is_active() {
                                    if ui.button("中止").clicked() {
                                        action = Some(JobAction::Cancel(row.id));
                                    }
                                    ui.spinner();
                                } else {
                                    if ui.button("再取得").clicked() {
                                        action = Some(JobAction::Retry(row.package.clone()));
                                    }
                                    if let Some(file) = row.files.first() {
                                        if ui.button("フォルダー").clicked()
                                            && let Some(dir) = file.parent()
                                        {
                                            action =
                                                Some(JobAction::OpenFolder(dir.to_path_buf()));
                                        }
                                        if ui.button("開く").clicked() {
                                            action = Some(JobAction::OpenFile(file.clone()));
                                        }
                                    }
                                }
                                ui.label(hint(ui, &row.created));
                            });
                        });
                    });

                if response.response.interact(egui::Sense::click()).clicked() {
                    action = Some(JobAction::Select(row.id));
                }
            }
        });

        if let Some(action) = action {
            self.apply_job_action(action);
        }
    }

    /// 選択したジョブの出力。何が起きているかを隠さない。
    ///
    /// 中央の一覧は残りの高さを全部使うので、その中に置くと出力が画面の外へ押し出される。
    /// 実際に取得が失敗したとき、原因の書かれた行がどこにも見えなかった。
    /// 下部の領域として**先に**確保しておく。
    fn job_log_panel(&mut self, ui: &mut egui::Ui) {
        if self.tab != Tab::Queue {
            return;
        }
        let Some(id) = self.selected_job else { return };
        let log = {
            let Ok(shared) = self.engine.shared.lock() else { return };
            match shared.job(id) {
                Some(job) => job.log.clone(),
                None => return,
            }
        };
        if log.is_empty() {
            return;
        }

        egui::Panel::bottom(Id::new("apkd_job_log")).show(ui, |ui| self.job_log(ui, &log));
    }

    fn job_log(&mut self, ui: &mut egui::Ui, log: &[String]) {
        ui.horizontal(|ui| {
            let label = if self.show_log { "▼ 出力" } else { "▶ 出力" };
            if ui.selectable_label(self.show_log, label).clicked() {
                self.show_log = !self.show_log;
            }
            ui.label(hint(ui, &format!("{} 行", log.len())));
            if ui.small_button("コピー").clicked() {
                ui.ctx().copy_text(log.join("\n"));
            }
        });

        if self.show_log {
            egui::ScrollArea::vertical()
                .id_salt("apkd_job_log_lines")
                .max_height(160.0)
                // 高さは中身に合わせて縮める。ここを false にすると領域を確保できない
                // 下部パネルの中で高さが定まらなくなる。
                .auto_shrink([false, true])
                .stick_to_bottom(true)
                .show(ui, |ui| {
                    for line in log {
                        ui.label(RichText::new(line).monospace().small());
                    }
                });
        }
    }

    fn apply_job_action(&mut self, action: JobAction) {
        match action {
            JobAction::Select(id) => {
                self.selected_job = Some(id);
                // 選んだジョブのアプリを情報欄にも出す。
                if let Ok(shared) = self.engine.shared.lock()
                    && let Some(job) = shared.job(id)
                {
                    let package = job.package.clone();
                    drop(shared);
                    self.engine.inspect(&package);
                }
            }
            JobAction::Cancel(id) => {
                if let Ok(shared) = self.engine.shared.lock()
                    && let Some(job) = shared.job(id)
                {
                    job.cancel();
                }
            }
            JobAction::OpenFolder(dir) => self.open_path(&dir),
            JobAction::OpenFile(file) => self.open_path(&file),
            JobAction::Retry(package) => {
                self.engine.enqueue(&package, None);
                self.tab = Tab::Queue;
            }
        }
    }

    fn history_list(&mut self, ui: &mut egui::Ui) {
        if self.history.is_empty() {
            ui.add_space(24.0);
            ui.vertical_centered(|ui| {
                ui.label(hint(ui, "取得した APK はまだありません。"));
            });
            return;
        }

        // `self` を丸ごと借りると icons と history が衝突する。分解して別々に借りる。
        let App { history, icons, .. } = self;
        let mut action = None;

        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            for (index, entry) in history.entries().iter().enumerate() {
                egui::Frame::group(ui.style()).fill(ui.visuals().faint_bg_color).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        if let Some(texture) = icons.get(&entry.icon_url) {
                            ui.add(
                                egui::Image::new(&texture)
                                    .fit_to_exact_size(egui::vec2(36.0, 36.0)),
                            );
                        } else {
                            ui.allocate_space(egui::vec2(36.0, 36.0));
                        }

                        ui.vertical(|ui| {
                            ui.horizontal(|ui| {
                                ui.label(RichText::new(entry.title()).strong());
                                if !entry.version.is_empty() {
                                    ui.label(hint(ui, &format!("v{}", entry.version)));
                                }
                                ui.label(hint(ui, entry.source.label()));
                                if !entry.exists() {
                                    ui.label(
                                        RichText::new("ファイルなし")
                                            .color(Color32::from_rgb(200, 130, 60))
                                            .small(),
                                    );
                                }
                            });
                            let names: Vec<String> = entry
                                .files
                                .iter()
                                .filter_map(|p| p.file_name())
                                .map(|n| n.to_string_lossy().into_owned())
                                .collect();
                            ui.label(hint(ui, &names.join(", ")));
                            ui.label(hint(
                                ui,
                                &format!(
                                    "{}  {}",
                                    entry.finished_at.format("%Y-%m-%d %H:%M"),
                                    format_size(entry.total_bytes)
                                ),
                            ));
                        });

                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if ui.button("削除").on_hover_text("履歴から消す (ファイルは残る)").clicked() {
                                action = Some(HistoryAction::Remove(index));
                            }
                            if ui.button("再取得").clicked() {
                                action = Some(HistoryAction::Retry(entry.package.clone()));
                            }
                            if ui.button("情報").clicked() {
                                action = Some(HistoryAction::Inspect(entry.package.clone()));
                            }
                            if let Some(dir) = entry.folder()
                                && ui.button("フォルダー").clicked()
                            {
                                action = Some(HistoryAction::OpenFolder(dir.to_path_buf()));
                            }
                            if let Some(file) = entry.files.first()
                                && ui.button("開く").clicked()
                            {
                                action = Some(HistoryAction::OpenFile(file.clone()));
                            }
                        });
                    });
                });
            }
        });

        match action {
            Some(HistoryAction::Remove(index)) => self.history.remove(index),
            Some(HistoryAction::Retry(package)) => {
                self.engine.enqueue(&package, None);
                self.tab = Tab::Queue;
            }
            Some(HistoryAction::Inspect(package)) => {
                self.input = package.clone();
                self.engine.inspect(&package);
            }
            Some(HistoryAction::OpenFolder(dir)) => self.open_path(&dir),
            Some(HistoryAction::OpenFile(file)) => self.open_path(&file),
            None => {}
        }
    }

    // ------------------------------------------------------------- 情報欄

    fn info_panel(&mut self, ui: &mut egui::Ui) {
        let (package, running, info, error) = {
            let Ok(state) = self.engine.inspect.lock() else { return };
            (state.package.clone(), state.running, state.info.clone(), state.error.clone())
        };

        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new("アプリ情報").strong());
            if running {
                ui.spinner();
            }
        });
        ui.separator();

        if package.is_empty() {
            ui.add_space(12.0);
            ui.label(hint(ui, "URL かパッケージ ID を入れると、ここに情報が出ます。"));
            return;
        }

        if let Some(message) = &error {
            ui.label(RichText::new(message).color(Color32::from_rgb(200, 90, 80)));
            ui.add_space(6.0);
            ui.label(hint(ui, "取得元にファイルがあれば、情報が取れなくてもダウンロードは試せます。"));
        }

        let Some(info) = info else {
            if !running && error.is_none() {
                ui.label(hint(ui, "情報を取得していません。"));
            }
            return;
        };

        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.horizontal(|ui| {
                if let Some(url) = &info.icon_url
                    && let Some(texture) = self.icons.get(url)
                {
                    ui.add(egui::Image::new(&texture).fit_to_exact_size(egui::vec2(72.0, 72.0)));
                }
                ui.vertical(|ui| {
                    ui.label(RichText::new(info.title()).heading());
                    if let Some(developer) = &info.developer {
                        ui.label(developer);
                    }
                    if let Some(score) = info.score {
                        ui.label(hint(ui, &format!("評価 {score:.2}")));
                    }
                });
            });

            ui.add_space(8.0);
            info_row(ui, "パッケージ", Some(&info.package));
            // Play は多くのアプリで版を載せていない。「取得できず」だと不具合に見えるので、
            // 事実 (ストアが出していない) と次の手 (取得元の一覧) を伝える。
            ui.horizontal(|ui| {
                ui.add_sized([110.0, 18.0], egui::Label::new(hint(ui, "バージョン")));
                match info.version.as_deref() {
                    Some(version) => {
                        ui.add(egui::Label::new(version).truncate());
                    }
                    None => {
                        ui.label(hint(ui, "ストアが公開していません"))
                            .on_hover_text("上の「一覧」を押すと、取得元にある版を調べられます。");
                    }
                }
            });
            info_row(ui, "更新日", info.updated_text.as_deref());
            info_row(ui, "インストール数", info.installs.as_deref());
            info_row(ui, "年齢区分", info.content_rating.as_deref());
            info_row(ui, "カテゴリ", info.genre.as_deref());
            info_row(ui, "価格", info.price_text.as_deref());
            info_row(ui, "対応 Android", info.android_version.as_deref());
            info_row(ui, "配信状態", Some(&format!("{} のストアで配信中", info.region)));

            if info.detail_fields_missing() {
                ui.add_space(6.0);
                ui.label(
                    RichText::new(
                        "バージョンと更新日を読み取れませんでした。Play のページ構造が\
                         変わった可能性があります (取得そのものには影響しません)。",
                    )
                    .color(Color32::from_rgb(200, 150, 60))
                    .small(),
                );
            }

            ui.add_space(10.0);
            ui.horizontal_wrapped(|ui| {
                if ui.button("ストアで開く").clicked() {
                    let url = info.url.clone();
                    self.open_url(&url);
                }
                if ui.button("この版を取得").clicked() {
                    let package = info.package.clone();
                    self.engine.enqueue(&package, None);
                    self.tab = Tab::Queue;
                }
                if ui.button("地域を確認").clicked() {
                    let package = info.package.clone();
                    self.engine.check_regions(&package);
                    self.show_region = true;
                }
            });

            if let Some(description) = &info.description {
                ui.add_space(10.0);
                ui.separator();
                ui.label(RichText::new("説明").strong());
                ui.label(hint(ui, &elide(description, 1200)));
            }
        });
    }

    pub fn open_url(&mut self, url: &str) {
        if let Err(e) = open::that(url) {
            self.set_notice(format!("ブラウザーを開けません: {e}"), false);
        }
    }
}

fn info_row(ui: &mut egui::Ui, label: &str, value: Option<&str>) {
    ui.horizontal(|ui| {
        ui.add_sized([110.0, 18.0], egui::Label::new(hint(ui, label)));
        match value {
            // 取れなかった項目をそれと分かる形で出す。空欄だと「無い」のか
            // 「読めなかった」のか区別できない。
            None => {
                ui.label(hint(ui, "取得できず"));
            }
            Some(value) => {
                ui.add(egui::Label::new(value).truncate());
            }
        }
    });
}

/// 長い文字列を丸める。文字境界で切るので日本語でも壊れない。
pub fn elide(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let head: String = s.chars().take(max_chars).collect();
    format!("{head}…")
}

pub fn format_size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}
