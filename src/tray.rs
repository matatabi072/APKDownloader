//! タスクトレイ常駐。
//!
//! アイコンは外部ファイルにせず状態ごとに描き起こす。単体 EXE 配布のため
//! リソースを増やしたくないのと、状態を色で即座に伝えたいため。
//!
//! Windows ではトレイアイコンはメッセージループを持つスレッドで作る必要がある。
//! ここは eframe のメインスレッド (winit のイベントループ) から呼ぶこと。

use std::sync::{Arc, Mutex};

use tray_icon::menu::{Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder, TrayIconEvent};

/// トレイから飛んでくる要求。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Command {
    Show,
    OpenFolder,
    OpenSettings,
    Exit,
}

/// トレイアイコンの見た目。取得中かどうかが一目で分かればよい。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Look {
    Idle,
    Working,
    Failed,
}

impl Look {
    fn color(self) -> [u8; 3] {
        match self {
            Self::Idle => [90, 130, 175],
            Self::Working => [60, 165, 95],
            Self::Failed => [200, 80, 70],
        }
    }
}

pub struct Tray {
    _icon: TrayIcon,
    pending: Arc<Mutex<Vec<Command>>>,
    last_tooltip: String,
    last_look: Option<Look>,
}

impl Tray {
    /// `on_exit` は「終了」を受け取った瞬間にトレイ監視スレッドから呼ばれる。
    /// UI スレッドが動いていなくても終了できるようにするための入口。
    pub fn new(
        title: &str,
        ctx: egui::Context,
        on_exit: impl Fn() + Send + 'static,
    ) -> Option<Self> {
        let item_show = MenuItem::new("表示", true, None);
        let item_folder = MenuItem::new("保存フォルダーを開く", true, None);
        let item_settings = MenuItem::new("設定", true, None);
        let item_exit = MenuItem::new("終了", true, None);

        let menu = Menu::new();
        menu.append_items(&[
            &item_show,
            &PredefinedMenuItem::separator(),
            &item_folder,
            &item_settings,
            &PredefinedMenuItem::separator(),
            &item_exit,
        ])
        .ok()?;

        let icon = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_tooltip(title)
            .with_icon(draw_icon(Look::Idle)?)
            .build()
            .ok()?;

        // メニュー操作は専用スレッドで拾う。
        //
        // UI スレッドで毎フレーム引き取る作りだと、イベントループが止まっている間
        // (トレイメニューのモーダルループ中など) クリックが宙に浮く。
        let pending = Arc::new(Mutex::new(Vec::new()));
        spawn_event_pump(
            Arc::clone(&pending),
            MenuIds {
                show: item_show.id().clone(),
                folder: item_folder.id().clone(),
                settings: item_settings.id().clone(),
                exit: item_exit.id().clone(),
            },
            ctx,
            on_exit,
        );

        Some(Self { _icon: icon, pending, last_tooltip: String::new(), last_look: None })
    }

    /// 見た目を状態に合わせる。値が変わったときだけ触る。
    pub fn update(&mut self, look: Look, tooltip: &str) {
        if self.last_look != Some(look) {
            self.last_look = Some(look);
            if let Some(icon) = draw_icon(look) {
                let _ = self._icon.set_icon(Some(icon));
            }
        }
        if self.last_tooltip != tooltip {
            self.last_tooltip = tooltip.to_string();
            let _ = self._icon.set_tooltip(Some(tooltip));
        }
    }

    /// 溜まっている操作を引き取る。毎フレーム呼ぶ。
    pub fn poll(&self) -> Vec<Command> {
        match self.pending.lock() {
            Ok(mut pending) => std::mem::take(&mut *pending),
            Err(_) => Vec::new(),
        }
    }
}

struct MenuIds {
    show: MenuId,
    folder: MenuId,
    settings: MenuId,
    exit: MenuId,
}

fn spawn_event_pump(
    pending: Arc<Mutex<Vec<Command>>>,
    ids: MenuIds,
    ctx: egui::Context,
    on_exit: impl Fn() + Send + 'static,
) {
    std::thread::Builder::new()
        .name("tray-events".into())
        .spawn(move || {
            loop {
                let mut received = Vec::new();

                while let Ok(event) = MenuEvent::receiver().try_recv() {
                    let command = if event.id == ids.show {
                        Command::Show
                    } else if event.id == ids.folder {
                        Command::OpenFolder
                    } else if event.id == ids.settings {
                        Command::OpenSettings
                    } else if event.id == ids.exit {
                        Command::Exit
                    } else {
                        continue;
                    };
                    // 終了だけはここで火を付ける。UI スレッドが眠っていても終われるように。
                    if command == Command::Exit {
                        on_exit();
                    }
                    received.push(command);
                }

                // アイコンのダブルクリックで表示 (Windows の慣習に合わせる)。
                while let Ok(event) = TrayIconEvent::receiver().try_recv() {
                    if let TrayIconEvent::DoubleClick { .. } = event {
                        received.push(Command::Show);
                    }
                }

                if !received.is_empty() {
                    if let Ok(mut queue) = pending.lock() {
                        queue.extend(received);
                    }
                    // 積むだけでは足りない。イベントループは入力が無いと眠ったままで、
                    // 「押しても反応しない」に見える。ここから明示的に起こす。
                    ctx.request_repaint_of(egui::ViewportId::ROOT);
                }

                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        })
        .expect("トレイ監視スレッドの起動に失敗");
}

/// 32x32 の下向き矢印 (ダウンロード) を描く。
fn draw_icon(look: Look) -> Option<Icon> {
    const N: i32 = 32;
    let [r, g, b] = look.color();
    let center = (N as f32 - 1.0) / 2.0;

    let mut rgba = vec![0u8; (N * N * 4) as usize];
    for y in 0..N {
        for x in 0..N {
            let (dx, dy) = (x as f32 - center, y as f32 - center);
            if (dx * dx + dy * dy).sqrt() > 15.0 {
                continue; // 円の外は透明のまま
            }

            // 軸 (縦棒)、矢じり (下向きの三角)、受け皿 (下辺) を白で抜く。
            let shaft = (14..=17).contains(&x) && (7..=17).contains(&y);
            let head = (17..=21).contains(&y) && {
                let half = 21 - y; // y が下がるほど幅が狭くなる
                (center as i32 - half..=center as i32 + half + 1).contains(&x)
            };
            let tray = (24..=25).contains(&y) && (9..=22).contains(&x);

            let px = if shaft || head || tray { [245, 245, 245, 255] } else { [r, g, b, 255] };
            let i = ((y * N + x) * 4) as usize;
            rgba[i..i + 4].copy_from_slice(&px);
        }
    }

    Icon::from_rgba(rgba, N as u32, N as u32).ok()
}
