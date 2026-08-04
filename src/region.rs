//! 地域・配信制限の確認。
//!
//! ストアページを国ごとに引き直し、返ってきたステータスと中身を並べる。
//!
//! **判定できることと、できないことを混ぜない。** ここで分かるのは
//! 「その国のストアにページが出ているか」と「出ているなら版・価格・年齢区分は何か」
//! だけ。端末非対応かどうかは端末のプロファイルを Play に送らないと決まらないので、
//! ここでは扱わず、画面にもその旨を明示する。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::playstore::{self, AppInfo};

/// 同時に叩く数。Play に短時間で大量に投げると 429 が返るので控えめにする。
const CONCURRENCY: usize = 4;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Availability {
    /// ページが返った。そのストアに出ている。
    Available,
    /// 404。その国のストアには出ていない。
    NotListed,
    /// 403 や 429。Google 側に断られた。配信状態の判断材料にはならない。
    Blocked,
    /// 通信そのものが失敗した。
    Error,
}

impl Availability {
    pub fn label(self) -> &'static str {
        match self {
            Self::Available => "配信中",
            Self::NotListed => "掲載なし",
            Self::Blocked => "確認不可",
            Self::Error => "エラー",
        }
    }

    pub fn color(self) -> egui::Color32 {
        match self {
            Self::Available => egui::Color32::from_rgb(60, 170, 90),
            Self::NotListed => egui::Color32::from_rgb(200, 80, 70),
            Self::Blocked => egui::Color32::from_rgb(215, 160, 40),
            Self::Error => egui::Color32::from_rgb(150, 150, 150),
        }
    }
}

#[derive(Clone, Debug)]
pub struct RegionStatus {
    pub country: String,
    pub http: u16,
    pub availability: Availability,
    pub version: Option<String>,
    pub price_text: Option<String>,
    pub content_rating: Option<String>,
    pub updated_text: Option<String>,
    pub name: Option<String>,
    pub error: Option<String>,
}

impl RegionStatus {
    /// その行の状態を一言で。画面の「理由」列に出す。
    pub fn note(&self) -> String {
        match self.availability {
            Availability::Available => match &self.content_rating {
                Some(rating) if is_age_restricted(rating) => {
                    format!("配信中 (年齢制限: {rating})")
                }
                _ => "配信中".to_string(),
            },
            Availability::NotListed => {
                "この国のストアに掲載なし (配信対象外・公開停止・ID 違いのいずれか)".to_string()
            }
            Availability::Blocked => {
                format!("Google に断られた (HTTP {}) ため判定できない", self.http)
            }
            Availability::Error => {
                self.error.clone().unwrap_or_else(|| "通信に失敗".to_string())
            }
        }
    }
}

/// 年齢区分の表記が「制限あり」に見えるか。
///
/// Play の区分名は国と言語で変わるので、数字と代表的な語だけを見る緩い判定にする。
/// 断定はせず、画面には注記として出す。
fn is_age_restricted(rating: &str) -> bool {
    const KEYWORDS: [&str; 6] = ["17", "18", "Mature", "Adults", "成人", "18 才"];
    KEYWORDS.iter().any(|k| rating.contains(k))
}

/// 確認結果の全体像。
#[derive(Clone, Debug)]
pub struct RegionReport {
    pub package: String,
    pub rows: Vec<RegionStatus>,
}

impl RegionReport {
    pub fn available_count(&self) -> usize {
        self.rows.iter().filter(|r| r.availability == Availability::Available).count()
    }

    /// 全体の見立て。行ごとの事実から導ける範囲だけを述べる。
    pub fn summary(&self) -> String {
        let available = self.available_count();
        let total = self.rows.len();
        if total == 0 {
            return "確認対象の国が指定されていません".to_string();
        }
        if available == 0 {
            let blocked = self
                .rows
                .iter()
                .filter(|r| r.availability == Availability::Blocked)
                .count();
            if blocked > 0 {
                return format!(
                    "{total} 国中 {blocked} 国で Google に確認を断られました。時間を空けて再確認してください"
                );
            }
            return format!(
                "確認した {total} 国すべてで掲載がありません。公開停止か、パッケージ ID が違う可能性があります"
            );
        }
        if available == total {
            return format!("確認した {total} 国すべてで配信中です");
        }
        let missing: Vec<&str> = self
            .rows
            .iter()
            .filter(|r| r.availability == Availability::NotListed)
            .map(|r| r.country.as_str())
            .collect();
        format!(
            "{total} 国中 {available} 国で配信中。掲載が無いのは {} です",
            missing.join(", ")
        )
    }

    /// バージョンが国によって違うか。違えば画面で注意を促す。
    pub fn versions_differ(&self) -> bool {
        let mut seen: Vec<&str> = self
            .rows
            .iter()
            .filter(|r| r.availability == Availability::Available)
            .filter_map(|r| r.version.as_deref())
            .collect();
        seen.sort_unstable();
        seen.dedup();
        seen.len() > 1
    }
}

/// 指定した国を順に (一部は並行で) 確認する。
///
/// `progress` は 1 国終わるたびに (完了数, 総数) で呼ばれる。画面の進捗表示用。
pub fn check(
    client: &reqwest::blocking::Client,
    package: &str,
    lang: &str,
    countries: &[String],
    cancel: &Arc<AtomicBool>,
    mut progress: impl FnMut(usize, usize) + Send,
) -> RegionReport {
    let total = countries.len();
    let results: Mutex<Vec<(usize, RegionStatus)>> = Mutex::new(Vec::with_capacity(total));
    let next = std::sync::atomic::AtomicUsize::new(0);
    let done = Mutex::new(0usize);
    let progress = Mutex::new(&mut progress);

    std::thread::scope(|scope| {
        for _ in 0..CONCURRENCY.min(total.max(1)) {
            scope.spawn(|| {
                loop {
                    if cancel.load(Ordering::Relaxed) {
                        return;
                    }
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    let Some(country) = countries.get(index) else { return };

                    let status = check_one(client, package, lang, country);

                    results.lock().expect("region results poisoned").push((index, status));
                    let mut done = done.lock().expect("region progress poisoned");
                    *done += 1;
                    (progress.lock().expect("region callback poisoned"))(*done, total);
                }
            });
        }
    });

    // 並行で走らせた分、指定順に戻してから返す。表の行が毎回入れ替わると読みにくい。
    let mut rows = results.into_inner().expect("region results poisoned");
    rows.sort_by_key(|(index, _)| *index);

    RegionReport {
        package: package.to_string(),
        rows: rows.into_iter().map(|(_, status)| status).collect(),
    }
}

fn check_one(
    client: &reqwest::blocking::Client,
    package: &str,
    lang: &str,
    country: &str,
) -> RegionStatus {
    let mut status = RegionStatus {
        country: country.to_string(),
        http: 0,
        availability: Availability::Error,
        version: None,
        price_text: None,
        content_rating: None,
        updated_text: None,
        name: None,
        error: None,
    };

    match playstore::fetch_page(client, package, lang, country) {
        Ok(page) => {
            status.http = page.status;
            status.availability = match page.status {
                200 => Availability::Available,
                404 => Availability::NotListed,
                _ => Availability::Blocked,
            };
            if page.status == 200 {
                let info: AppInfo = playstore::parse_info(&page.body, package, country, &page.url);
                status.name = info.name;
                status.version = info.version;
                status.price_text = info.price_text;
                status.content_rating = info.content_rating;
                status.updated_text = info.updated_text;
            }
        }
        Err(e) => status.error = Some(e.to_string()),
    }

    status
}

/// 端末対応可否について画面に出す但し書き。
///
/// 仕様の「端末非対応」を勝手に判定した風に見せないため、文言を一箇所に置く。
pub const DEVICE_SUPPORT_NOTE: &str =
    "端末非対応かどうかは、実機のモデル・API レベル・ABI を Play に伝えないと判定できません。\
     ここでは扱わないため、この表には現れません。";

/// 「配信中」が何を意味するかの但し書き。
///
/// この表で分かるのは**ページの有無**であって、その国で入れられるかではない。
/// 実測 (2026-08) では Hulu も Google Pay India も 10 か国すべてでページが出た。
/// 「配信中」を「その国で使える」と読まれると事実を誤って伝えることになるので明示する。
pub const AVAILABILITY_MEANING_NOTE: &str =
    "「配信中」はその国のストアにページがあるという意味です。地域を絞っているアプリでも\
     ページ自体は他の国から見えることが多く、実際に入れられるかまでは分かりません。";
