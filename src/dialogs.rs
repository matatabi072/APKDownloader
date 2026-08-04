//! 設定ウィンドウと地域確認ウィンドウ。
//!
//! どちらも縦に長くなるので、中身は `ScrollArea` に入れ、確定ボタンはその**外**に置く。
//! 内側に入れると画面が低いときにボタンごと見切れて、閉じられなくなる。

use egui::{Align, Color32, Id, Layout, RichText};

use crate::app::App;
use crate::config::{Source, Theme};
use crate::region::{AVAILABILITY_MEANING_NOTE, DEVICE_SUPPORT_NOTE};
use crate::ui::{elide, hint};
use crate::{build_info, config, paths, provider};

impl App {
    // ---------------------------------------------------------------- 設定

    pub fn settings_window(&mut self, ctx: &egui::Context) {
        if !self.show_settings {
            return;
        }
        // 開いた瞬間に下書きが無ければ作る (トレイから直接開かれた場合)。
        if self.draft.is_none() {
            self.draft = Some(self.settings.clone());
            self.draft_region_text = self.settings.regions.join(", ");
        }

        // 0.29 の `screen_rect` は 0.35 で `content_rect` になった。
        let screen = ctx.content_rect();
        let body_height = (screen.height() - 220.0).max(240.0);

        let mut close = false;
        let mut apply = false;

        let response = egui::Modal::new(Id::new("apkd_settings")).show(ctx, |ui| {
            ui.set_width((screen.width() * 0.72).clamp(520.0, 780.0));
            ui.heading("設定");
            ui.separator();

            egui::ScrollArea::vertical()
                .max_height(body_height)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    self.settings_body(ui);
                });

            ui.separator();
            ui.horizontal(|ui| {
                ui.label(hint(ui, &format!("v{}", build_info::VERSION)));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.button("保存").clicked() {
                        apply = true;
                        close = true;
                    }
                    if ui.button("取り消し").clicked() {
                        close = true;
                    }
                });
            });
        });

        // 背景クリックや Esc も取り消し扱いにする。保存は明示的な操作だけにしたい。
        if response.should_close() {
            close = true;
        }

        if apply
            && let Some(mut draft) = self.draft.clone()
        {
            draft.regions = parse_regions(&self.draft_region_text);
            if draft.regions.is_empty() {
                draft.regions = config::default_regions();
            }
            self.apply_settings(draft);
            self.set_notice("設定を保存しました", true);
        }
        if close {
            self.show_settings = false;
            self.draft = None;
        }
    }

    fn settings_body(&mut self, ui: &mut egui::Ui) {
        // 下書きを取り出して編集し、最後に書き戻す。借用を短く保つため。
        let Some(mut draft) = self.draft.take() else { return };

        section(ui, "取得", |ui| {
            ui.horizontal(|ui| {
                ui.label("保存先");
                let mut text = draft.download_dir.display().to_string();
                if ui
                    .add(egui::TextEdit::singleline(&mut text).desired_width(360.0))
                    .changed()
                {
                    draft.download_dir = text.into();
                }
                if ui.button("参照…").clicked()
                    && let Some(dir) = rfd::FileDialog::new()
                        .set_title("APK の保存先")
                        .set_directory(&draft.download_dir)
                        .pick_folder()
                {
                    draft.download_dir = dir;
                }
            });

            ui.horizontal(|ui| {
                ui.label("取得元");
                egui::ComboBox::from_id_salt("apkd_settings_source")
                    .selected_text(draft.source.label())
                    .width(260.0)
                    .show_ui(ui, |ui| {
                        for candidate in Source::ALL {
                            ui.selectable_value(&mut draft.source, candidate, candidate.label());
                        }
                    });
            });
            ui.label(hint(
                ui,
                "APKPure はミラーサイトで、認証は要りません。Google Play を選ぶと\
                 自分のアカウントで取得します。F-Droid は OSS のみ扱えます。",
            ));

            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.label("apkeep");
                let mut text = draft.apkeep_path.clone();
                if ui
                    .add(
                        egui::TextEdit::singleline(&mut text)
                            .desired_width(360.0)
                            .hint_text(hint(ui, "空欄なら自動で探す")),
                    )
                    .changed()
                {
                    draft.apkeep_path = text;
                }
                if ui.button("参照…").clicked()
                    && let Some(file) = rfd::FileDialog::new()
                        .set_title("apkeep.exe")
                        .add_filter("実行ファイル", &["exe"])
                        .pick_file()
                {
                    draft.apkeep_path = file.display().to_string();
                }
            });
            match provider::resolve(&draft.apkeep_path) {
                Some(path) => {
                    ui.label(
                        RichText::new(format!("検出: {}", path.display()))
                            .color(Color32::from_rgb(60, 170, 90))
                            .small(),
                    );
                }
                None => {
                    ui.label(
                        RichText::new("見つかりません")
                            .color(Color32::from_rgb(200, 90, 80))
                            .small(),
                    );
                    ui.label(hint(ui, provider::SETUP_HINT));
                }
            }

            if draft.source.needs_credentials() {
                ui.add_space(6.0);
                ui.label(RichText::new("Google アカウント").strong());
                ui.label(hint(
                    ui,
                    "AAS トークンはパスワードではなく、apkeep が Play へ接続するための鍵です。\
                     取得方法は apkeep の USAGE-google-play.md に書かれています。\
                     この値は設定ファイルに平文で保存されるので、共有 PC では扱いに注意してください。",
                ));
                ui.horizontal(|ui| {
                    ui.label("メールアドレス");
                    ui.add(
                        egui::TextEdit::singleline(&mut draft.google_email).desired_width(300.0),
                    );
                });
                ui.horizontal(|ui| {
                    ui.label("AAS トークン");
                    ui.add(
                        egui::TextEdit::singleline(&mut draft.google_aas_token)
                            .password(true)
                            .desired_width(300.0),
                    );
                });
            }

            ui.checkbox(&mut draft.open_folder_on_complete, "取得が終わったら保存フォルダーを開く");
        });

        section(ui, "アプリ情報", |ui| {
            ui.horizontal(|ui| {
                ui.label("表示言語");
                ui.add(egui::TextEdit::singleline(&mut draft.language).desired_width(60.0))
                    .on_hover_text("ja / en など (ISO 639-1)");
                ui.add_space(12.0);
                ui.label("基準の国");
                ui.add(egui::TextEdit::singleline(&mut draft.primary_region).desired_width(60.0))
                    .on_hover_text("JP / US など (ISO 3166-1 alpha-2)");
            });
            ui.label(hint(ui, "情報欄に出す値は、この言語と国のストアページから取ります。"));

            ui.add_space(6.0);
            ui.label("地域確認で調べる国");
            ui.add(
                egui::TextEdit::multiline(&mut self.draft_region_text)
                    .desired_rows(2)
                    .desired_width(f32::INFINITY)
                    .hint_text(hint(ui, "JP, US, GB, DE …")),
            );
            ui.label(hint(ui, "カンマ区切り。多いほど確認に時間がかかります。"));
        });

        section(ui, "ブラウザー連携", |ui| {
            ui.checkbox(&mut draft.server_enabled, "拡張機能からの受け口を開く");
            ui.horizontal(|ui| {
                ui.label("ポート");
                let mut port = draft.server_port.to_string();
                if ui
                    .add(egui::TextEdit::singleline(&mut port).desired_width(80.0))
                    .changed()
                    && let Ok(value) = port.trim().parse::<u16>()
                    && value > 0
                {
                    draft.server_port = value;
                }
                ui.label(hint(ui, "127.0.0.1 のみ。外部からは届きません。"));
            });

            ui.horizontal(|ui| {
                ui.label("合言葉");
                ui.add(
                    egui::TextEdit::singleline(&mut draft.auth_token)
                        .desired_width(280.0)
                        .font(egui::TextStyle::Monospace),
                );
                if ui.button("コピー").clicked() {
                    ui.ctx().copy_text(draft.auth_token.clone());
                }
                if ui.button("作り直す").clicked() {
                    draft.auth_token = config::generate_token();
                }
            });
            ui.label(hint(
                ui,
                "同じ端末で動く他のプログラムからも受け口は見えます。合言葉が無いと、\
                 開いたページの JavaScript から勝手に取得を始められてしまいます。\
                 この値を拡張機能の設定画面に貼り付けてください。",
            ));

            ui.add_space(6.0);
            ui.checkbox(&mut draft.clipboard_watch, "コピーされた Play の URL を見張る");
            ui.label(hint(ui, "見つけても自動では取得しません。画面に候補として出すだけです。"));

            ui.checkbox(&mut draft.hotkey_enabled, "Ctrl + Shift + D でクリップボードから取得");
            if draft.hotkey_enabled && !self.hotkey_active() {
                ui.label(
                    RichText::new("このキーは他のアプリに取られていて登録できていません")
                        .color(Color32::from_rgb(200, 130, 60))
                        .small(),
                );
            }
        });

        section(ui, "常駐", |ui| {
            ui.checkbox(&mut draft.start_minimized, "起動時はトレイに格納した状態にする");
            ui.checkbox(&mut draft.close_to_tray, "✕ で終了せずトレイへしまう");
            ui.label(hint(ui, "終了はトレイアイコンの右クリックメニューから行えます。"));
            ui.checkbox(&mut draft.notify_on_complete, "取得の完了・失敗を通知する");
        });

        section(ui, "画面", |ui| {
            ui.horizontal(|ui| {
                ui.label("テーマ");
                egui::ComboBox::from_id_salt("apkd_theme")
                    .selected_text(draft.theme.label())
                    .show_ui(ui, |ui| {
                        for candidate in Theme::ALL {
                            ui.selectable_value(&mut draft.theme, candidate, candidate.label());
                        }
                    });
            });
            // 選んだ瞬間に見た目が変わる方が確かめやすい。保存前でも反映する。
            ui.ctx().set_theme(draft.theme.preference());
        });

        section(ui, "このアプリについて", |ui| {
            ui.horizontal(|ui| {
                ui.label(hint(ui, "設定と履歴の置き場"));
                ui.label(RichText::new(paths::data_dir().display().to_string()).monospace().small());
            });
            ui.horizontal(|ui| {
                if ui.button("データフォルダーを開く").clicked() {
                    let dir = paths::data_dir().to_path_buf();
                    self.open_path(&dir);
                }
                if ui.button("ログを開く").clicked()
                    && let Some(path) = crate::logfile::path()
                {
                    let path = path.to_path_buf();
                    self.open_path(&path);
                }
            });
            ui.label(hint(
                ui,
                "APK の取得は外部ツール apkeep (EFF・MIT) に任せています。\
                 このアプリは URL の解釈・情報表示・履歴・常駐を担当します。",
            ));
        });

        self.draft = Some(draft);
    }

    // ------------------------------------------------------------ 地域確認

    pub fn region_window(&mut self, ctx: &egui::Context) {
        if !self.show_region {
            return;
        }

        let (package, running, done, total, report, error) = {
            let Ok(state) = self.engine.region.lock() else { return };
            (
                state.package.clone(),
                state.running,
                state.done,
                state.total,
                state.report.clone(),
                state.error.clone(),
            )
        };

        let mut open = true;
        let mut cancel = false;
        let mut recheck = None;

        egui::Window::new("配信地域の確認")
            .open(&mut open)
            .default_size([760.0, 460.0])
            .collapsible(false)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(&package).strong().monospace());
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if running {
                            if ui.button("中止").clicked() {
                                cancel = true;
                            }
                            ui.spinner();
                            ui.label(hint(ui, &format!("{done} / {total}")));
                        } else if ui.button("もう一度確認").clicked() {
                            recheck = Some(package.clone());
                        }
                    });
                });

                if running && total > 0 {
                    ui.add(
                        egui::ProgressBar::new(done as f32 / total as f32)
                            .desired_height(6.0)
                            .animate(true),
                    );
                }
                ui.separator();

                if let Some(message) = &error {
                    ui.label(RichText::new(message).color(Color32::from_rgb(200, 130, 60)));
                }

                let Some(report) = &report else {
                    if running {
                        ui.label(hint(ui, "各国のストアを順に確認しています…"));
                    }
                    return;
                };

                ui.label(RichText::new(report.summary()).strong());
                if report.versions_differ() {
                    ui.label(
                        RichText::new("国によって配信バージョンが異なります。")
                            .color(Color32::from_rgb(200, 150, 60))
                            .small(),
                    );
                }
                ui.add_space(6.0);

                // 表だけを送る。但し書きと操作は外に置く。
                // 中に入れると、表の幅が見えている幅より広くなったときに文章が折り返さず
                // 右端で切れる (実際に切れていた)。
                egui::ScrollArea::vertical().max_height(360.0).auto_shrink([false, true]).show(
                    ui,
                    |ui| {
                    // TableBuilder は列幅やクリック判定で扱いが難しい。読み取り専用の
                    // 表なので Grid で十分。
                    egui::Grid::new("apkd_regions")
                        .num_columns(6)
                        .striped(true)
                        .spacing([14.0, 6.0])
                        .show(ui, |ui| {
                            for header in
                                ["国", "状態", "バージョン", "価格", "年齢区分", "備考"]
                            {
                                ui.label(RichText::new(header).strong().small());
                            }
                            ui.end_row();

                            for row in &report.rows {
                                ui.label(RichText::new(&row.country).monospace());
                                ui.label(
                                    RichText::new(row.availability.label())
                                        .color(row.availability.color()),
                                );
                                ui.label(row.version.as_deref().unwrap_or("—"));
                                ui.label(row.price_text.as_deref().unwrap_or("—"));
                                ui.label(row.content_rating.as_deref().unwrap_or("—"));
                                ui.add(egui::Label::new(elide(&row.note(), 46)).truncate());
                                ui.end_row();
                            }
                        });
                    },
                );

                ui.add_space(10.0);
                ui.separator();
                for note in [
                    AVAILABILITY_MEANING_NOTE,
                    DEVICE_SUPPORT_NOTE,
                    "「掲載なし」は配信対象外・公開停止・ID 違いのどれでも同じ 404 になります。\
                     一部の国だけ掲載が無ければ地域制限、全部無ければ公開停止か ID 違いです。",
                ] {
                    ui.add(egui::Label::new(hint(ui, note)).wrap());
                }

                ui.add_space(6.0);
                if ui.button("結果をコピー").clicked() {
                    ui.ctx().copy_text(report_to_text(report));
                }
            });

        if cancel && let Ok(state) = self.engine.region.lock() {
            state.cancel();
        }
        if let Some(package) = recheck {
            self.engine.check_regions(&package);
        }
        if !open {
            self.show_region = false;
        }
    }
}

/// 節の見出しと枠。設定画面が縦に伸びても区切りで読めるようにする。
fn section(ui: &mut egui::Ui, title: &str, contents: impl FnOnce(&mut egui::Ui)) {
    ui.add_space(8.0);
    ui.label(RichText::new(title).strong());
    egui::Frame::group(ui.style()).show(ui, |ui| {
        ui.set_width(ui.available_width());
        contents(ui);
    });
}

/// 「JP, us,GB」のような入力を国コードの並びに直す。
fn parse_regions(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for token in text.split([',', '\n', ' ', '\t', '、']) {
        let code = token.trim().to_uppercase();
        if code.len() == 2 && code.chars().all(|c| c.is_ascii_alphabetic()) && !out.contains(&code) {
            out.push(code);
        }
    }
    out
}

/// 表を貼り付けやすい形にする。
fn report_to_text(report: &crate::region::RegionReport) -> String {
    let mut text = format!("{}\n{}\n\n", report.package, report.summary());
    text.push_str("国\t状態\tバージョン\t価格\t年齢区分\n");
    for row in &report.rows {
        text.push_str(&format!(
            "{}\t{}\t{}\t{}\t{}\n",
            row.country,
            row.availability.label(),
            row.version.as_deref().unwrap_or("-"),
            row.price_text.as_deref().unwrap_or("-"),
            row.content_rating.as_deref().unwrap_or("-"),
        ));
    }
    text
}
