//! 完了通知。
//!
//! 常駐して裏で取得する以上、終わったことを画面外にも伝える必要がある。
//! 失敗しても本体の動作には影響させない — 通知が出ないより、通知のせいで
//! 取得が止まる方が困る。

/// トースト通知を出す。出せなくてもログに残すだけで済ませる。
pub fn toast(title: &str, body: &str) {
    // 本文が長いと通知が読みにくくなるので詰める。
    let body = truncate(body, 180);

    let result = notify_rust::Notification::new()
        .summary(title)
        .body(&body)
        .appname(crate::build_info::WINDOW_TITLE)
        .show();

    if let Err(e) = result {
        crate::logfile::write(format!("通知を出せない ({title}): {e}"));
    }
}

fn truncate(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let head: String = s.chars().take(max_chars).collect();
    format!("{head}…")
}
