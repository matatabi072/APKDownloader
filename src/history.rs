//! ダウンロード履歴。
//!
//! 「いつ・どのバージョンを・どこに落としたか」が後から追えれば十分なので、
//! DB は持たず 1 本の JSON に追記していく。

use std::path::{Path, PathBuf};

use chrono::{DateTime, Local};
use serde::{Deserialize, Serialize};

use crate::config::Source;
use crate::paths;

/// 履歴に残す件数の上限。これを超えたら古い方から捨てる。
const MAX_ENTRIES: usize = 500;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Entry {
    pub package: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub developer: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub icon_url: String,
    pub source: Source,
    pub finished_at: DateTime<Local>,
    /// 取得できたファイル。分割 APK だと複数になる。
    #[serde(default)]
    pub files: Vec<PathBuf>,
    #[serde(default)]
    pub total_bytes: u64,
}

impl Entry {
    /// 一覧に出す見出し。アプリ名が取れていなければパッケージ ID で代替する。
    pub fn title(&self) -> &str {
        if self.name.is_empty() { &self.package } else { &self.name }
    }

    /// ファイルが今も存在するか。履歴から開く前に確認するために使う。
    pub fn exists(&self) -> bool {
        self.files.iter().any(|p| p.exists())
    }

    /// 「フォルダーを開く」の対象。
    pub fn folder(&self) -> Option<&Path> {
        self.files.first().and_then(|p| p.parent())
    }
}

#[derive(Default)]
pub struct History {
    entries: Vec<Entry>,
}

impl History {
    pub fn path() -> PathBuf {
        paths::data_dir().join("history.json")
    }

    pub fn load() -> Self {
        let Ok(text) = std::fs::read_to_string(Self::path()) else {
            return Self::default();
        };
        match serde_json::from_str::<Vec<Entry>>(paths::strip_bom(&text)) {
            Ok(entries) => Self { entries },
            Err(e) => {
                crate::logfile::write(format!("履歴を読めないので空で始める: {e}"));
                Self::default()
            }
        }
    }

    /// 新しい順に並んだ一覧。画面はこの順で出す。
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn push(&mut self, entry: Entry) {
        self.entries.insert(0, entry);
        self.entries.truncate(MAX_ENTRIES);
        self.save();
    }

    pub fn remove(&mut self, index: usize) {
        if index < self.entries.len() {
            self.entries.remove(index);
            self.save();
        }
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.save();
    }

    fn save(&self) {
        let path = Self::path();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        match serde_json::to_string_pretty(&self.entries) {
            Ok(text) => {
                if let Err(e) = std::fs::write(&path, text) {
                    crate::logfile::write(format!("履歴を保存できない: {e}"));
                }
            }
            Err(e) => crate::logfile::write(format!("履歴を JSON にできない: {e}")),
        }
    }
}
