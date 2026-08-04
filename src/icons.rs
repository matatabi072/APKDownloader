//! アプリアイコンの取得と表示。
//!
//! egui_extras の画像ローダーは使わず自前で持つ。読み込みの成否と再試行を
//! こちらで制御したいのと、依存を増やさないため。

use std::collections::HashMap;
use std::sync::{mpsc, Arc, Mutex};

use egui::{ColorImage, TextureHandle, TextureOptions};

enum Slot {
    Loading,
    Ready(TextureHandle),
    Failed,
}

/// 取り込み済みのアイコンを URL で引く置き場。
pub struct IconCache {
    slots: HashMap<String, Slot>,
    tx: mpsc::Sender<(String, Option<ColorImage>)>,
    rx: mpsc::Receiver<(String, Option<ColorImage>)>,
    client: Arc<reqwest::blocking::Client>,
    /// 同時に走らせる取得数の上限。一覧に大量のアイコンが並んでも回線を埋めない。
    in_flight: Arc<Mutex<usize>>,
    /// 画像が届いたことを画面へ知らせるための入口。
    ctx: egui::Context,
}

/// Play のアイコン URL は末尾のサイズ指定で解像度が変わる。表示に足る大きさに揃える。
const TARGET_SIZE: u32 = 128;

impl IconCache {
    pub fn new(client: Arc<reqwest::blocking::Client>, ctx: egui::Context) -> Self {
        let (tx, rx) = mpsc::channel();
        Self {
            slots: HashMap::new(),
            tx,
            rx,
            client,
            in_flight: Arc::new(Mutex::new(0)),
            ctx,
        }
    }

    /// 届いた画像をテクスチャに変える。毎フレーム先頭で呼ぶ。
    pub fn pump(&mut self, ctx: &egui::Context) {
        while let Ok((url, image)) = self.rx.try_recv() {
            let slot = match image {
                Some(image) => {
                    Slot::Ready(ctx.load_texture(&url, image, TextureOptions::default()))
                }
                None => Slot::Failed,
            };
            self.slots.insert(url, slot);
        }
    }

    /// アイコンを引く。無ければ取得を始めて `None` を返す。
    pub fn get(&mut self, url: &str) -> Option<TextureHandle> {
        if url.is_empty() {
            return None;
        }
        match self.slots.get(url) {
            Some(Slot::Ready(texture)) => return Some(texture.clone()),
            Some(_) => return None,
            None => {}
        }

        self.slots.insert(url.to_string(), Slot::Loading);
        self.spawn_fetch(url.to_string());
        None
    }

    fn spawn_fetch(&self, url: String) {
        const MAX_IN_FLIGHT: usize = 4;
        {
            let mut count = self.in_flight.lock().expect("icon counter poisoned");
            if *count >= MAX_IN_FLIGHT {
                // 詰まっているときは諦めて、次に画面へ出たときにまた試す。
                return;
            }
            *count += 1;
        }

        let client = Arc::clone(&self.client);
        let tx = self.tx.clone();
        let in_flight = Arc::clone(&self.in_flight);
        let ctx = self.ctx.clone();

        std::thread::spawn(move || {
            let image = fetch_image(&client, &sized_url(&url));
            if let Ok(mut count) = in_flight.lock() {
                *count = count.saturating_sub(1);
            }
            let _ = tx.send((url, image));
            // 送るだけでは足りない。`pump` は描画のときにしか回らず、egui は入力が
            // 無いと眠っているので、画面を触らずにいるとアイコンが出ないままになる。
            // 取得中は 250ms ごとの再描画で隠れるが、静かなときに露見する。
            ctx.request_repaint();
        });
    }
}

/// Play のアイコン URL に大きさ指定を足す。
///
/// `=w240-h480` のような接尾辞が既に付いていることがあるので、その場合は置き換える。
fn sized_url(url: &str) -> String {
    let base = url.split_once("=w").map(|(head, _)| head).unwrap_or(url);
    let base = base.split_once("=s").map(|(head, _)| head).unwrap_or(base);
    format!("{base}=s{TARGET_SIZE}")
}

fn fetch_image(client: &reqwest::blocking::Client, url: &str) -> Option<ColorImage> {
    let response = client.get(url).send().ok()?;
    if !response.status().is_success() {
        return None;
    }
    let bytes = response.bytes().ok()?;
    let decoded = image::load_from_memory(&bytes).ok()?.to_rgba8();
    let size = [decoded.width() as usize, decoded.height() as usize];
    Some(ColorImage::from_rgba_unmultiplied(size, decoded.as_raw()))
}
