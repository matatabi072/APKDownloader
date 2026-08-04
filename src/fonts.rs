//! 日本語フォントの読み込み。
//!
//! egui の既定フォントには日本語グリフが無く、何もしないと全部豆腐になる。
//! 単体 EXE 方針なのでフォントは埋め込まず、Windows 標準フォントを実行時に読む。

use std::sync::Arc;

use egui::{FontData, FontDefinitions, FontFamily};

/// 読み込みを試す Windows 標準フォント。上から順に、最初に見つかったものを使う。
const CANDIDATES: [&str; 3] = [
    "YuGothM.ttc",  // 游ゴシック Medium (Win 8.1 以降)
    "meiryo.ttc",   // メイリオ
    "msgothic.ttc", // MS ゴシック (最後の砦)
];

/// 日本語フォントを最優先の候補として差し込む。読めなければ何もしない。
pub fn install_japanese(ctx: &egui::Context) -> Option<String> {
    let dir = std::env::var_os("WINDIR").map(std::path::PathBuf::from)?.join("Fonts");

    let (name, bytes) = CANDIDATES.iter().find_map(|file| {
        let bytes = std::fs::read(dir.join(file)).ok()?;
        Some((file.to_string(), bytes))
    })?;

    let mut defs = FontDefinitions::default();
    defs.font_data.insert(name.clone(), Arc::new(FontData::from_owned(bytes)));

    // 先頭に入れる。後ろに足すと ASCII を既定フォントが拾い、和文だけ別書体になる。
    for family in [FontFamily::Proportional, FontFamily::Monospace] {
        defs.families.entry(family).or_default().insert(0, name.clone());
    }

    ctx.set_fonts(defs);
    Some(name)
}
