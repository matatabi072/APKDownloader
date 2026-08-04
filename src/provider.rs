//! APK の取得部。外部の `apkeep` を子プロセスとして駆動する。
//!
//! **なぜ自前で取りに行かないか。** Google Play には APK を配る公開 API が無く、
//! 取得するには端末を騙る非公開プロトコルを実装するか、ミラーサイトを叩くしかない。
//! どちらも「どこから・どういう資格で取るか」を利用者が選べる形にしておきたい。
//! `apkeep` (EFF, MIT) はその選択肢を CLI として持っているので、ここでは
//! 「どこから取るか」を任せ、こちらは URL の解釈・情報表示・履歴・常駐に専念する。
//!
//! 副次的な利点として、取得元の仕様変更に追随する責任がこのアプリから外れる。

use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;

use crate::config::Source;

/// APK として扱う拡張子。分割 APK は xapk / apkm でまとめて配られる。
const APK_EXTENSIONS: [&str; 4] = ["apk", "xapk", "apkm", "apks"];

#[derive(Clone, Debug, Default)]
pub struct Credentials {
    pub email: String,
    pub aas_token: String,
}

impl Credentials {
    pub fn is_complete(&self) -> bool {
        !self.email.trim().is_empty() && !self.aas_token.trim().is_empty()
    }
}

/// `apkeep` の実行ファイルを探す。
///
/// 設定で明示されていればそれを最優先。無ければ Portable 運用に合わせて
/// EXE の隣 → `cargo install` の既定位置 → PATH の順で見る。
pub fn resolve(configured: &str) -> Option<PathBuf> {
    let configured = configured.trim();
    if !configured.is_empty() {
        let path = PathBuf::from(configured);
        return path.is_file().then_some(path);
    }

    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(dir) = crate::paths::exe_dir() {
        candidates.push(dir.join("apkeep.exe"));
        candidates.push(dir.join("tools").join("apkeep.exe"));
    }
    if let Some(home) = std::env::var_os("USERPROFILE") {
        candidates.push(PathBuf::from(home).join(".cargo").join("bin").join("apkeep.exe"));
    }
    if let Some(found) = candidates.into_iter().find(|p| p.is_file()) {
        return Some(found);
    }

    which_on_path("apkeep.exe").or_else(|| which_on_path("apkeep"))
}

/// PATH を自分でたどる。専用クレートを足すほどの処理ではない。
fn which_on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|dir| dir.join(name)).find(|p| p.is_file())
}

/// 取得の実行に必要なものが揃った状態。
///
/// 実行ファイル・取得元・資格情報は常に 3 つ揃って意味を持つので、ばらばらに
/// 引き回さず 1 つにまとめる。`prepare` を通った時点で「実行できる」ことが型で分かる。
pub struct Backend {
    pub exe: PathBuf,
    pub source: Source,
    pub credentials: Credentials,
}

/// 取得元と設定の噛み合わせを事前に見る。実行してから落ちるより早く伝えたい。
pub fn prepare(
    apkeep_path: &str,
    source: Source,
    credentials: &Credentials,
) -> Result<Backend, String> {
    let Some(exe) = resolve(apkeep_path) else {
        return Err(
            "apkeep が見つかりません。設定画面から実行ファイルを指定するか、\
             `cargo install apkeep` で導入してください。"
                .to_string(),
        );
    };
    if source.needs_credentials() && !credentials.is_complete() {
        return Err(
            "Google Play から取得するには、設定画面でメールアドレスと AAS トークンの\
             登録が必要です。取得元を APKPure に変えることもできます。"
                .to_string(),
        );
    }
    Ok(Backend { exe, source, credentials: credentials.clone() })
}

/// 取得の結果。
pub struct Outcome {
    /// 新しくできたファイル。分割 APK だと複数になることがある。
    pub files: Vec<PathBuf>,
    pub total_bytes: u64,
}

/// APK を取得する。
///
/// `on_line` には子プロセスの出力が 1 行ずつ渡る。`apkeep` は進捗率を機械可読な形で
/// 出さないため、パーセンテージは作らず出力そのものを画面に流す。
/// 数字をでっち上げるより、実際に何が起きているかを見せる方が役に立つ。
pub fn download(
    backend: &Backend,
    package: &str,
    version: Option<&str>,
    out_dir: &Path,
    cancel: &Arc<AtomicBool>,
    mut on_line: impl FnMut(String),
) -> anyhow::Result<Outcome> {
    std::fs::create_dir_all(out_dir)?;
    let before = snapshot_dir(out_dir);

    let app_arg = match version {
        Some(v) if !v.trim().is_empty() => format!("{package}@{}", v.trim()),
        _ => package.to_string(),
    };

    let mut command = backend.base_command();
    command.arg("-a").arg(&app_arg);
    command.arg(out_dir);

    on_line(format!("apkeep -a {app_arg} -d {}", backend.source.cli_value()));

    // 失敗したときに理由をそのまま見せたいので、最後の 1 行を控えておく。
    // apkeep は原因を最終行に書く (例: "Could not verify F-Droid package index. Exiting.")。
    // 終了コードだけ伝えても利用者には何も分からない。
    let mut last_line = String::new();
    let exit = run_streaming(command, cancel, &mut |line| {
        last_line = line.clone();
        on_line(line);
    })?;

    if cancel.load(Ordering::Relaxed) {
        anyhow::bail!("利用者の操作で中止しました");
    }
    if !exit.success {
        anyhow::bail!(
            "apkeep が失敗しました (終了コード {}){}",
            exit.code.map(|c| c.to_string()).unwrap_or_else(|| "不明".into()),
            if last_line.is_empty() {
                "。出力はありませんでした。".to_string()
            } else {
                format!(": {last_line}")
            }
        );
    }

    let files = new_files(out_dir, &before);
    if files.is_empty() {
        // 増えていない理由は 2 つある。取得元に無かったか、**同じ名前のファイルが
        // 既にあって apkeep が飛ばしたか**。apkeep 1.0.0 はどちらも
        // `Downloading <pkg>...` と出して終了コード 0 で終わるので、出力では区別できない
        // (実測 2026-08)。保存先を見て自分で見分ける。
        //
        // 見分けないと、一度取得したアプリを取り直そうとしただけで
        // 「取得元にこのアプリが無いようです」と嘘を返すことになる。実際そうなっていた。
        if let Some(existing) = existing_for(out_dir, package).into_iter().next() {
            if looks_complete(&existing) {
                on_line(format!(
                    "保存先に同じ名前のファイルがあるため、apkeep は取得を飛ばしました。既にあるものを使います: {}",
                    existing.display()
                ));
                let total_bytes = std::fs::metadata(&existing).map(|m| m.len()).unwrap_or(0);
                return Ok(Outcome { files: vec![existing], total_bytes });
            }
            anyhow::bail!(
                "保存先に途中までのファイルが残っているため、apkeep が取得を飛ばしました: {}。\
                 中断した取得の残りです。このファイルを削除してからやり直してください。",
                existing.display()
            );
        }

        // apkeep 1.0.0 は「取得元に無かった」ときも**終了コード 0 で黙って終わる**
        // (実測 2026-08: 存在しないパッケージ・存在しない版のどちらも 0)。
        // 終了コードを信じると成功扱いになるので、ファイルが増えたかどうかで判定する。
        anyhow::bail!(
            "apkeep は正常終了しましたが、{} に新しい APK がありません。{}",
            out_dir.display(),
            match version {
                Some(v) if !v.trim().is_empty() => format!(
                    "取得元にバージョン {} が無いようです。「一覧」で取得できる版を確認してください。",
                    v.trim()
                ),
                _ => "取得元にこのアプリが無いようです。パッケージ ID と取得元を確認してください。"
                    .to_string(),
            }
        );
    }

    let total_bytes = files
        .iter()
        .filter_map(|p| std::fs::metadata(p).ok())
        .map(|m| m.len())
        .sum();

    Ok(Outcome { files, total_bytes })
}

/// 取得できるバージョンの一覧。
pub fn list_versions(
    backend: &Backend,
    package: &str,
    cancel: &Arc<AtomicBool>,
) -> anyhow::Result<Vec<String>> {
    let mut command = backend.base_command();
    command.arg("-a").arg(package);
    command.arg("-l");

    let mut lines = Vec::new();
    let exit = run_streaming(command, cancel, &mut |line| lines.push(line))?;
    if !exit.success {
        anyhow::bail!("バージョン一覧を取得できませんでした:\n{}", lines.join("\n"));
    }

    Ok(parse_version_list(&lines))
}

/// `apkeep -l` の出力からバージョン文字列を抜く。
///
/// 出力の形は取得元で変わる。`pkg | 1.0, 2.0` の形と、1 行 1 バージョンの形の
/// 両方を受け入れる。読み取れない行は捨てるだけにして、失敗にはしない。
fn parse_version_list(lines: &[String]) -> Vec<String> {
    let mut versions = Vec::new();
    for line in lines {
        let body = line.split_once('|').map(|(_, rest)| rest).unwrap_or(line);
        for token in body.split([',', ' ', '\t']) {
            let token = token.trim().trim_matches(['"', '\'']);
            if token.is_empty() || !token.starts_with(|c: char| c.is_ascii_digit()) {
                continue;
            }
            if !versions.iter().any(|v: &String| v == token) {
                versions.push(token.to_string());
            }
        }
    }
    versions
}

impl Backend {
    /// 取得元と資格情報まで設定済みの `apkeep` 呼び出し。
    /// 引数の付け忘れが起きないよう、組み立てを 1 箇所に閉じる。
    fn base_command(&self) -> Command {
        let mut command = Command::new(&self.exe);
        command.stdout(Stdio::piped()).stderr(Stdio::piped()).stdin(Stdio::null());
        command.arg("-d").arg(self.source.cli_value());
        if self.source.needs_credentials() {
            command.arg("-e").arg(self.credentials.email.trim());
            command.arg("-t").arg(self.credentials.aas_token.trim());
        }
        hide_console(&mut command);
        command
    }
}

/// 子プロセスのコンソール窓を出さない。
///
/// これが無いと、取得のたびに黒い窓が一瞬点滅する。常駐アプリとしては目障りなので抑える。
#[cfg(windows)]
fn hide_console(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn hide_console(_command: &mut Command) {}

/// 子プロセスをアプリと運命を共にさせる。
///
/// **Windows は親が終わっても子を止めない。** 実測 (2026-08): 取得中にアプリを終了した
/// あとも apkeep が走り続け、26 秒後まで保存先のファイルが増え続けた (0 → 98 MB)。
/// 画面もトレイアイコンも消えているので、利用者から見れば「終了したのに回線と
/// ディスクを使い続ける正体不明のプロセス」で、止める手立てがタスク マネージャーしかない。
///
/// `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` を立てたジョブへ入れておくと、こちらの
/// ハンドルが閉じた時点 — 通常終了だけでなく強制終了や異常終了も含む — で OS が
/// 確実に始末する。終了処理を書き足す方法だと、落ち方によっては素通りしてしまう。
#[cfg(windows)]
fn tie_to_app(child: &Child) {
    use std::os::windows::io::AsRawHandle;
    use std::sync::OnceLock;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
        SetInformationJobObject,
    };

    // ジョブは 1 つだけ作り、以後の子を全部そこへ入れる。
    // ハンドルは意図的に閉じない。閉じた瞬間に子が死ぬので、プロセスが終わるまで持ち続ける。
    static JOB: OnceLock<Option<isize>> = OnceLock::new();
    let job = *JOB.get_or_init(|| unsafe {
        let job = CreateJobObjectW(None, None)
            .inspect_err(|e| crate::logfile::write(format!("ジョブを作れない: {e}")))
            .ok()?;

        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            (&raw const limits).cast(),
            size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        )
        .inspect_err(|e| crate::logfile::write(format!("ジョブの設定に失敗: {e}")))
        .ok()?;

        Some(job.0 as isize)
    });

    let Some(job) = job else { return };
    let handle = HANDLE(child.as_raw_handle());
    if let Err(e) = unsafe { AssignProcessToJobObject(HANDLE(job as *mut _), handle) } {
        // 入れられなくても取得自体は続けられる。終了時に残るだけなので、記録して進む。
        crate::logfile::write(format!("子プロセスをジョブに入れられない: {e}"));
    }
}

#[cfg(not(windows))]
fn tie_to_app(_child: &Child) {}

struct Exit {
    success: bool,
    code: Option<i32>,
}

/// 子プロセスを走らせ、出力を 1 行ずつ流しながら終了を待つ。
///
/// 標準出力と標準エラーは別々に読む必要がある。片方だけ読んでいると、
/// もう片方のパイプが詰まったところで子プロセスが停止する。
fn run_streaming(
    mut command: Command,
    cancel: &Arc<AtomicBool>,
    on_line: &mut impl FnMut(String),
) -> anyhow::Result<Exit> {
    let mut child: Child = command.spawn().map_err(|e| {
        anyhow::anyhow!("apkeep を起動できません: {e}")
    })?;
    tie_to_app(&child);

    let (tx, rx) = mpsc::channel::<String>();
    for pipe in [
        child.stdout.take().map(PipeSource::Out),
        child.stderr.take().map(PipeSource::Err),
    ]
    .into_iter()
    .flatten()
    {
        let tx = tx.clone();
        std::thread::spawn(move || pipe.pump(tx));
    }
    // 送信側の複製を全部落としておかないと、子が終わっても rx が閉じない。
    drop(tx);

    loop {
        // 出力待ちで固まらないよう、短い待ちを挟んで中止要求と終了を見に行く。
        match rx.recv_timeout(Duration::from_millis(150)) {
            Ok(line) => {
                on_line(line);
                continue;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                let exit = child.wait()?;
                return Ok(Exit { success: exit.success(), code: exit.code() });
            }
        }

        if cancel.load(Ordering::Relaxed) {
            let _ = child.kill();
            let _ = child.wait();
            return Ok(Exit { success: false, code: None });
        }

        if let Some(exit) = child.try_wait()? {
            // 終了直後にまだパイプへ残っている分を取り切る。
            while let Ok(line) = rx.try_recv() {
                on_line(line);
            }
            return Ok(Exit { success: exit.success(), code: exit.code() });
        }
    }
}

enum PipeSource {
    Out(std::process::ChildStdout),
    Err(std::process::ChildStderr),
}

impl PipeSource {
    fn pump(self, tx: mpsc::Sender<String>) {
        let reader: Box<dyn BufRead> = match self {
            Self::Out(out) => Box::new(BufReader::new(out)),
            Self::Err(err) => Box::new(BufReader::new(err)),
        };
        for line in reader.lines().map_while(Result::ok) {
            let line = line.trim_end().to_string();
            if line.is_empty() {
                continue;
            }
            if tx.send(line).is_err() {
                return;
            }
        }
    }
}

/// 取得前のフォルダーの中身。差分から「今回できたファイル」を割り出す。
fn snapshot_dir(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    entries.flatten().map(|e| e.path()).collect()
}

fn new_files(dir: &Path, before: &[PathBuf]) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut files: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| !before.contains(p))
        .filter(|p| is_apk_file(p))
        .collect();
    files.sort();
    files
}

fn is_apk_file(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| APK_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

/// 保存先に既にある、このアプリのファイル。
///
/// apkeep は `<パッケージ>.apk`、版を指定したときは `<パッケージ>@<版>.apk` の名前で置く。
fn existing_for(dir: &Path, package: &str) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let prefix = format!("{package}@");
    let mut files: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| is_apk_file(p))
        .filter(|p| {
            p.file_stem()
                .and_then(|s| s.to_str())
                .map(|stem| stem == package || stem.starts_with(&prefix))
                .unwrap_or(false)
        })
        .collect();
    files.sort();
    files
}

/// zip として最後まで揃っているか。
///
/// 中断された取得は途中で切れた zip として残る。APK も XAPK も中身は zip なので、
/// 終端記録 (EOCD, `PK\x05\x06`) があるかどうかで完結しているかを見分けられる。
/// 中身が正しいアプリかまでは見ない。「途中で切れている」ことが分かれば足りる。
fn looks_complete(path: &Path) -> bool {
    let Ok(mut file) = std::fs::File::open(path) else { return false };
    let Ok(len) = file.metadata().map(|m| m.len()) else { return false };
    // EOCD は最小 22 バイト。後ろにコメントが最大 65535 バイト付くので、その分まで遡る。
    const EOCD_MIN: u64 = 22;
    const MAX_COMMENT: u64 = 65535;
    if len < EOCD_MIN {
        return false;
    }

    let window = len.min(EOCD_MIN + MAX_COMMENT);
    if file.seek(SeekFrom::End(-(window as i64))).is_err() {
        return false;
    }
    let mut tail = vec![0u8; window as usize];
    if file.read_exact(&mut tail).is_err() {
        return false;
    }
    tail.windows(4).any(|w| w == [0x50, 0x4b, 0x05, 0x06])
}

/// apkeep の入手方法。設定画面にそのまま出す。
pub const SETUP_HINT: &str = concat!(
    "apkeep は APK の取得を担う外部ツールです (EFF 製・MIT)。\n",
    "・Rust があれば: cargo install apkeep\n",
    "・配布物を使う場合: https://github.com/EFForg/apkeep/releases から apkeep.exe を取得し、\n",
    "  この EXE と同じフォルダー (または tools フォルダー) に置くと自動で見つかります。"
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_pipe_separated_versions() {
        let lines = vec!["com.example.app | 1.0.0, 1.2.3, 2.0".to_string()];
        assert_eq!(parse_version_list(&lines), vec!["1.0.0", "1.2.3", "2.0"]);
    }

    /// apkeep 1.0.0 が APKPure に対して実際に返す形 (実測 2026-08)。
    /// 見出し行が混ざり、バージョン行は `|` で始まって前にパッケージ名が無い。
    #[test]
    fn parses_real_apkeep_output() {
        let lines = vec![
            "Versions available for org.videolan.vlc on APKPure:".to_string(),
            "| 1, 2, 3, 3.3.2, 3.4.0, 3.7.1, 4, 5, 6".to_string(),
        ];
        assert_eq!(
            parse_version_list(&lines),
            vec!["1", "2", "3", "3.3.2", "3.4.0", "3.7.1", "4", "5", "6"]
        );
    }

    /// 検証用の作業フォルダー。テストごとに別名にする。
    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("apkd_test_{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("作業フォルダーを作れること");
        dir
    }

    /// 中身が空でも成立する最小の zip (終端記録だけ)。
    const EMPTY_ZIP: [u8; 22] = [
        0x50, 0x4b, 0x05, 0x06, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    ];

    #[test]
    fn finds_the_file_for_this_package() {
        let dir = temp_dir("existing");
        for name in [
            "com.example.app.apk",
            "com.example.app@1.2.3.apk",
            "com.example.apple.apk", // 前方一致に釣られてはいけない
            "com.example.app.txt",   // APK ではない
        ] {
            std::fs::write(dir.join(name), b"x").unwrap();
        }

        let found = existing_for(&dir, "com.example.app");
        let names: Vec<String> =
            found.iter().map(|p| p.file_name().unwrap().to_string_lossy().into_owned()).collect();
        assert_eq!(names, vec!["com.example.app.apk", "com.example.app@1.2.3.apk"]);
    }

    /// 中断されたファイルと、取得しきったファイルを見分けられること。
    /// これができないと、中断の残骸を「取得済み」として履歴へ入れてしまう。
    #[test]
    fn tells_a_truncated_file_from_a_complete_one() {
        let dir = temp_dir("complete");

        let whole = dir.join("whole.apk");
        std::fs::write(&whole, EMPTY_ZIP).unwrap();
        assert!(looks_complete(&whole));

        let cut = dir.join("cut.apk");
        std::fs::write(&cut, vec![0u8; 5000]).unwrap();
        assert!(!looks_complete(&cut));

        let empty = dir.join("empty.apk");
        std::fs::write(&empty, b"").unwrap();
        assert!(!looks_complete(&empty));

        assert!(!looks_complete(&dir.join("no-such-file.apk")));
    }

    /// 実物の apkeep を叩く。導入済みの環境でのみ意味があるので既定では走らせない。
    /// `cargo test -- --ignored` で確認する。
    #[test]
    #[ignore = "apkeep の導入と通信が要る"]
    fn lists_real_versions() {
        let backend = prepare("", Source::ApkPure, &Credentials::default())
            .expect("apkeep が見つからない");
        let cancel = Arc::new(AtomicBool::new(false));
        let versions = list_versions(&backend, "org.videolan.vlc", &cancel).unwrap();
        assert!(!versions.is_empty(), "版が 1 つも読めていない");
        assert!(
            versions.iter().all(|v| v.starts_with(|c: char| c.is_ascii_digit())),
            "版でない文字列が混ざっている: {versions:?}"
        );
    }

    #[test]
    fn parses_one_version_per_line_and_dedups() {
        let lines = vec![
            "Looking up versions".to_string(),
            "1.0.0".to_string(),
            "1.0.0".to_string(),
            "2.5.1".to_string(),
        ];
        assert_eq!(parse_version_list(&lines), vec!["1.0.0", "2.5.1"]);
    }
}
