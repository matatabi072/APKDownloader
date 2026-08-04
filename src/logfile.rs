//! ファイルログ。
//!
//! `windows_subsystem = "windows"` でコンソールが無いため、`eprintln!` は消える。
//! 調査に必要な足跡はすべてここへ流す。

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

static PATH: OnceLock<PathBuf> = OnceLock::new();
static LOCK: Mutex<()> = Mutex::new(());

/// ログの出力先を決める。`paths::data_dir()` が確定した直後に一度だけ呼ぶ。
pub fn init(dir: &Path) {
    let _ = PATH.set(dir.join("apk-downloader.log"));
}

pub fn write(message: impl AsRef<str>) {
    let Some(path) = PATH.get() else { return };
    let line = format!(
        "{} {}\n",
        chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f"),
        message.as_ref()
    );

    // 書き込み中に別スレッドが割り込むと行が混ざる。ロックで直列化する。
    let _guard = LOCK.lock();
    if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = file.write_all(line.as_bytes());
    }
}

/// ログが肥大しないよう、起動時に上限を超えていたら切り詰める。
pub fn trim_if_large() {
    const LIMIT: u64 = 2 * 1024 * 1024;
    let Some(path) = PATH.get() else { return };
    let Ok(meta) = std::fs::metadata(path) else { return };
    if meta.len() <= LIMIT {
        return;
    }

    let _guard = LOCK.lock();
    if let Ok(text) = std::fs::read_to_string(path) {
        let _ = std::fs::write(path, tail_half(&text));
    }
}

/// 後ろ半分だけ残す。切り口は文字の境界と行頭に合わせる。
///
/// **`split_at(len / 2)` を使ってはいけない。** 文字の途中を指すと panic する。
/// ログは日本語なので 1 文字 3 バイトで、半分の位置が境界に乗るかは運任せ
/// (実測: 2000 通りのうち 1000 通りが非境界)。ここで落ちると、ログが上限を
/// 超えた次の起動からアプリが**黙って起動しなくなる**。コンソールが無いので
/// 画面にもログにも何も残らず、原因に辿り着けない。
fn tail_half(text: &str) -> &str {
    let mut cut = text.len() / 2;
    while cut < text.len() && !text.is_char_boundary(cut) {
        cut += 1;
    }
    let keep = &text[cut..];
    // 行の途中から始まらないよう、最初の改行まで捨てる。
    keep.find('\n').map(|i| &keep[i + 1..]).unwrap_or(keep)
}

pub fn path() -> Option<&'static Path> {
    PATH.get().map(|p| p.as_path())
}

#[cfg(test)]
mod tests {
    use super::tail_half;

    /// 半分より後ろを、行の頭から残す。
    /// 半分の位置がちょうど行頭でも、その行は捨てる (境界の判定を増やす価値がない)。
    #[test]
    fn keeps_the_tail() {
        let text = "1 行目\n2 行目\n3 行目\n4 行目\n";
        assert_eq!(tail_half(text), "4 行目\n");
    }

    /// 日本語のログで、半分の位置が文字の途中になる場合。以前はここで panic していた。
    #[test]
    fn survives_a_cut_inside_a_character() {
        let line = "2026-08-04 12:00:00.000 ジョブ #1 完了: アプリを取得しました\n";
        for n in 1..=200 {
            let text = line.repeat(n);
            let keep = tail_half(&text);
            assert!(text.ends_with(keep), "{n} 行のとき末尾が一致しない");
        }
    }

    /// 改行が無ければ切り詰めるだけ。ここでも文字は割らない。
    #[test]
    fn handles_a_single_line_without_a_newline() {
        assert_eq!(tail_half("改行のない一行"), "い一行");
    }
}
