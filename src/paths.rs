//! 保存先の決定。Portable 運用が第一なので、まず EXE の隣を試す。

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::build_info;

static DATA_DIR: OnceLock<PathBuf> = OnceLock::new();

/// 設定・履歴・ログの置き場。
///
/// EXE の隣 (`<exe>/data`) に書ければそこを使う。USB メモリーに入れて持ち運んだとき
/// 設定ごと付いてくるのが Portable の要点。書けない場所 (Program Files 配下など) に
/// 置かれたときだけ `%APPDATA%` へ逃がす。
pub fn data_dir() -> &'static Path {
    DATA_DIR.get_or_init(|| {
        if let Some(dir) = exe_dir() {
            let candidate = dir.join(build_info::DATA_DIR_NAME);
            if is_writable(&candidate) {
                return candidate;
            }
        }

        let roaming = std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir)
            .join("APKDownloader")
            .join(build_info::DATA_DIR_NAME);
        let _ = std::fs::create_dir_all(&roaming);
        roaming
    })
}

pub fn exe_dir() -> Option<PathBuf> {
    std::env::current_exe().ok()?.parent().map(PathBuf::from)
}

/// 既定のダウンロード先。`<data>/apk` ではなく EXE の隣の `APK` にする。
/// 取り出しやすさを優先し、設定フォルダーの中には入れない。
pub fn default_download_dir() -> PathBuf {
    if let Some(dir) = exe_dir() {
        let candidate = dir.join("APK");
        if is_writable(&candidate) {
            return candidate;
        }
    }
    dirs_download().unwrap_or_else(std::env::temp_dir).join("APKDownloader")
}

fn dirs_download() -> Option<PathBuf> {
    // 既知フォルダー API を引かずに済ませる。日本語環境でも %USERPROFILE%\Downloads は
    // 実体として存在する (表示名だけが「ダウンロード」になる)。
    let profile = std::env::var_os("USERPROFILE").map(PathBuf::from)?;
    let downloads = profile.join("Downloads");
    downloads.is_dir().then_some(downloads)
}

/// 先頭の BOM を落とす。
///
/// Portable 運用では利用者が設定ファイルを直接編集する。Windows のメモ帳や
/// PowerShell の `Set-Content -Encoding utf8` は UTF-8 BOM を付けて保存するが、
/// JSON の仕様に BOM は無く `serde_json` は先頭の `\u{feff}` で解析に失敗する。
/// 実際にこれで設定が丸ごと既定値へ戻る事故を起こしたので、読み込み時に取り除く。
pub fn strip_bom(text: &str) -> &str {
    text.strip_prefix('\u{feff}').unwrap_or(text)
}

/// 実際に作って書いてみる。属性だけでは判定できない (仮想化やアクセス権で化ける)。
fn is_writable(dir: &Path) -> bool {
    if std::fs::create_dir_all(dir).is_err() {
        return false;
    }
    let probe = dir.join(".write-probe");
    match std::fs::write(&probe, b"") {
        Ok(()) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_utf8_bom() {
        assert_eq!(strip_bom("\u{feff}{\"a\":1}"), "{\"a\":1}");
        assert_eq!(strip_bom("{\"a\":1}"), "{\"a\":1}");
        // BOM が無い日本語で先頭を削らないこと。
        assert_eq!(strip_bom("あ"), "あ");
    }
}
