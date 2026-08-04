//! クリップボード監視。ブラウザー拡張を入れない場合の代替手段。
//!
//! **勝手に取得は始めない。** クリップボードは利用者が他の目的でも使う場所なので、
//! Play の URL をコピーしただけで通信が走ると驚きになる。ここでは「候補」を
//! 画面へ差し出すだけにして、開始するかは利用者が決める。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::playstore;

/// 見張りの間隔。人が貼り付けてから気付くまでの体感と、無駄な起床の兼ね合い。
const POLL: Duration = Duration::from_millis(800);

#[derive(Default)]
pub struct Suggestion {
    /// 直近に見つけたパッケージ ID。利用者が受け入れるか却下するまで残す。
    pub package: Option<String>,
    /// 元の文字列。画面に出して確認してもらう。
    pub source_text: String,
}

pub struct Watcher {
    pub enabled: Arc<AtomicBool>,
    pub suggestion: Arc<Mutex<Suggestion>>,
    /// 一度断られた文字列を覚えておき、同じものを何度も出さない。
    dismissed: Arc<Mutex<String>>,
}

impl Watcher {
    pub fn start(ctx: egui::Context, enabled: bool) -> Self {
        let watcher = Self {
            enabled: Arc::new(AtomicBool::new(enabled)),
            suggestion: Arc::new(Mutex::new(Suggestion::default())),
            dismissed: Arc::new(Mutex::new(String::new())),
        };

        let flag = Arc::clone(&watcher.enabled);
        let suggestion = Arc::clone(&watcher.suggestion);
        let dismissed = Arc::clone(&watcher.dismissed);

        std::thread::Builder::new()
            .name("clipboard-watch".into())
            .spawn(move || {
                // クリップボードのハンドルは使うたびに開き直す。開きっぱなしにすると
                // 他のアプリのコピー操作を妨げることがある。
                let mut last_seen = String::new();
                loop {
                    std::thread::sleep(POLL);
                    if !flag.load(Ordering::Relaxed) {
                        continue;
                    }

                    let Some(text) = read_clipboard() else { continue };
                    if text == last_seen || text.len() > 2048 {
                        continue;
                    }
                    last_seen.clone_from(&text);

                    if dismissed.lock().map(|d| *d == text).unwrap_or(false) {
                        continue;
                    }
                    // Play のページを指していることを確かめる。パッケージ ID 単体は
                    // 受け付けない。ただの文字列と区別できず、誤検知が多くなる。
                    if !text.contains("play.google.com") && !text.starts_with("market://") {
                        continue;
                    }
                    let Some(package) = playstore::parse_package_id(&text) else { continue };

                    if let Ok(mut suggestion) = suggestion.lock() {
                        suggestion.package = Some(package);
                        suggestion.source_text = text;
                    }
                    ctx.request_repaint();
                }
            })
            .expect("クリップボード監視スレッドの起動に失敗");

        watcher
    }

    pub fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::Relaxed);
        if !enabled {
            self.clear();
        }
    }

    pub fn peek(&self) -> Option<(String, String)> {
        let suggestion = self.suggestion.lock().ok()?;
        let package = suggestion.package.clone()?;
        Some((package, suggestion.source_text.clone()))
    }

    /// 断られた候補を覚えて消す。同じ内容がクリップボードに残っていても再提示しない。
    pub fn dismiss(&self) {
        if let Ok(mut suggestion) = self.suggestion.lock() {
            if let Ok(mut dismissed) = self.dismissed.lock() {
                dismissed.clone_from(&suggestion.source_text);
            }
            suggestion.package = None;
            suggestion.source_text.clear();
        }
    }

    pub fn clear(&self) {
        if let Ok(mut suggestion) = self.suggestion.lock() {
            suggestion.package = None;
            suggestion.source_text.clear();
        }
    }
}

/// いま入っている文字列を読む。読めない形式 (画像など) のときは黙って諦める。
pub fn read_clipboard() -> Option<String> {
    let mut clipboard = arboard::Clipboard::new().ok()?;
    let text = clipboard.get_text().ok()?;
    let text = text.trim().to_string();
    (!text.is_empty()).then_some(text)
}
