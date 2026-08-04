//! ブラウザー拡張からの受け口。
//!
//! 127.0.0.1 のみに束縛し、外部からは届かないようにする。そのうえで合言葉を要求する。
//! localhost に開いた口は、同じ端末で動く**あらゆる**プロセス — 閲覧中のページが
//! 走らせる JavaScript も含む — から叩けてしまう。無認証だと、ページを開いただけで
//! 任意のアプリをダウンロードさせられる。合言葉は拡張の設定画面で貼り付けてもらう。
//!
//! 極小の用途なので非同期ランタイムは持ち込まず、`tiny_http` を専用スレッドで回す。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use tiny_http::{Header, Method, Request, Response, Server};

use crate::build_info;
use crate::jobs::Engine;

/// 画面に出す稼働状態。
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum Status {
    #[default]
    Stopped,
    Listening(u16),
    Error(String),
}

impl Status {
    pub fn label(&self) -> String {
        match self {
            Self::Stopped => "停止中".to_string(),
            Self::Listening(port) => format!("待受中 127.0.0.1:{port}"),
            Self::Error(e) => format!("起動できず: {e}"),
        }
    }

    pub fn is_listening(&self) -> bool {
        matches!(self, Self::Listening(_))
    }
}

pub struct ServerHandle {
    port: u16,
    token: String,
    stop: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
    pub status: Arc<Mutex<Status>>,
}

impl ServerHandle {
    /// 設定が変わったとき、立て直しが要るか。
    pub fn matches(&self, port: u16, token: &str) -> bool {
        self.port == port && self.token == token
    }

    pub fn status(&self) -> Status {
        self.status.lock().map(|s| s.clone()).unwrap_or_default()
    }
}

impl Drop for ServerHandle {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        // join はしない。受信待ちの残り時間 (最大 300ms) を終了時に待たせたくない。
        // スレッドはフラグを見て自分で畳む。
        drop(self.join.take());
    }
}

/// サーバーを立てる。束縛に失敗しても呼び出し側は落とさず、状態として持つ。
pub fn start(port: u16, token: String, engine: Arc<Engine>) -> ServerHandle {
    let status = Arc::new(Mutex::new(Status::Stopped));
    let stop = Arc::new(AtomicBool::new(false));

    let join = {
        let status = Arc::clone(&status);
        let stop = Arc::clone(&stop);
        let token = token.clone();
        std::thread::Builder::new()
            .name("apk-ipc".into())
            .spawn(move || serve(port, token, engine, status, stop))
            .ok()
    };

    ServerHandle { port, token, stop, join, status }
}

fn serve(
    port: u16,
    token: String,
    engine: Arc<Engine>,
    status: Arc<Mutex<Status>>,
    stop: Arc<AtomicBool>,
) {
    // 0.0.0.0 ではなく 127.0.0.1。LAN の他機から叩ける口にはしない。
    let server = match Server::http(("127.0.0.1", port)) {
        Ok(server) => server,
        Err(e) => {
            let message = e.to_string();
            crate::logfile::write(format!("受け口を開けない (ポート {port}): {message}"));
            if let Ok(mut status) = status.lock() {
                *status = Status::Error(message);
            }
            return;
        }
    };

    crate::logfile::write(format!("受け口を開いた: 127.0.0.1:{port}"));
    if let Ok(mut status) = status.lock() {
        *status = Status::Listening(port);
    }

    loop {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        // 受信待ちで固まると停止要求に気付けない。短い待ちで区切る。
        match server.recv_timeout(Duration::from_millis(300)) {
            Ok(Some(request)) => handle(request, &token, &engine),
            Ok(None) => {}
            Err(e) => {
                crate::logfile::write(format!("受信に失敗: {e}"));
                break;
            }
        }
    }

    crate::logfile::write("受け口を閉じた");
    if let Ok(mut status) = status.lock() {
        *status = Status::Stopped;
    }
}

fn handle(mut request: Request, token: &str, engine: &Engine) {
    let method = request.method().clone();
    // クエリを落として経路だけ見る。
    let url = request.url().to_string();
    let (path, query) = url.split_once('?').unwrap_or((url.as_str(), ""));
    let path = path.trim_end_matches('/');

    // 拡張の fetch は事前検査 (preflight) を飛ばしてくる。これに答えないと本体が届かない。
    if method == Method::Options {
        respond(request, 204, "");
        return;
    }

    match (&method, path) {
        (Method::Get, "" | "/ping") => {
            // 合言葉なしで答える。拡張が「アプリが起きているか」を確かめる口なので、
            // ここで漏れる情報 (名前と版) は害にならない。
            respond(
                request,
                200,
                &format!(
                    r#"{{"ok":true,"app":"APKDownloader","version":"{}","debug":{}}}"#,
                    build_info::VERSION,
                    build_info::IS_DEBUG
                ),
            );
        }

        (Method::Post, "/enqueue") | (Method::Get, "/enqueue") => {
            let supplied = header_value(&request, "x-apkd-token")
                .or_else(|| query_value(query, "token"))
                .unwrap_or_default();
            if !constant_time_eq(&supplied, token) {
                crate::logfile::write("合言葉が違う要求を拒否した");
                respond(request, 403, r#"{"ok":false,"error":"invalid token"}"#);
                return;
            }

            let input = if method == Method::Get {
                query_value(query, "url").unwrap_or_default()
            } else {
                let mut body = String::new();
                if std::io::Read::read_to_string(request.as_reader(), &mut body).is_err() {
                    respond(request, 400, r#"{"ok":false,"error":"body unreadable"}"#);
                    return;
                }
                json_string_field(&body, "url").unwrap_or(body)
            };

            match engine.enqueue_input(input.trim(), None) {
                Ok(id) => {
                    crate::logfile::write(format!("拡張から受信: {input} → ジョブ #{id}"));
                    respond(request, 200, &format!(r#"{{"ok":true,"job":{id}}}"#));
                }
                Err(message) => {
                    crate::logfile::write(format!("拡張から受け取った値を解釈できない: {message}"));
                    respond(
                        request,
                        400,
                        &format!(r#"{{"ok":false,"error":{}}}"#, json_quote(&message)),
                    );
                }
            }
        }

        // トレイ格納中に EXE をもう一度起動したとき、2 つ目のインスタンスがここを叩く。
        // 外から `ShowWindow` で出すと winit の内部状態とずれるので、表示は本人にやらせる。
        (Method::Post, "/show") | (Method::Get, "/show") => {
            let supplied = header_value(&request, "x-apkd-token")
                .or_else(|| query_value(query, "token"))
                .unwrap_or_default();
            if !constant_time_eq(&supplied, token) {
                crate::logfile::write("合言葉が違う要求を拒否した");
                respond(request, 403, r#"{"ok":false,"error":"invalid token"}"#);
                return;
            }
            engine.show_requested.store(true, std::sync::atomic::Ordering::Relaxed);
            engine.wake();
            respond(request, 200, r#"{"ok":true}"#);
        }

        _ => respond(request, 404, r#"{"ok":false,"error":"not found"}"#),
    }
}

fn respond(request: Request, status: u16, body: &str) {
    let mut response = Response::from_string(body).with_status_code(status);
    for (name, value) in [
        ("Content-Type", "application/json; charset=utf-8"),
        // 拡張のバックグラウンドからの fetch を通す。localhost 限定 + 合言葉が
        // 実質の防壁なので、生成元は絞らない (拡張の生成元は端末ごとに変わる)。
        ("Access-Control-Allow-Origin", "*"),
        ("Access-Control-Allow-Methods", "GET, POST, OPTIONS"),
        ("Access-Control-Allow-Headers", "Content-Type, X-APKD-Token"),
        ("Access-Control-Max-Age", "600"),
    ] {
        if let Ok(header) = Header::from_bytes(name.as_bytes(), value.as_bytes()) {
            response.add_header(header);
        }
    }
    let _ = request.respond(response);
}

fn header_value(request: &Request, name: &str) -> Option<String> {
    request
        .headers()
        .iter()
        .find(|h| h.field.as_str().as_str().eq_ignore_ascii_case(name))
        .map(|h| h.value.as_str().to_string())
}

fn query_value(query: &str, key: &str) -> Option<String> {
    query.split('&').find_map(|pair| {
        let (k, v) = pair.split_once('=')?;
        (k == key).then(|| urlencoding::decode(v).map(|s| s.into_owned()).unwrap_or_default())
    })
}

/// 依存を増やさずに JSON から 1 つの文字列項目を取る。
///
/// 受け取るのは拡張が作った `{"url": "..."}` だけなので、これで足りる。
fn json_string_field(body: &str, key: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()?
        .get(key)?
        .as_str()
        .map(|s| s.to_string())
}

fn json_quote(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".to_string())
}

/// 合言葉の比較。長さの違いと不一致位置を実行時間から読めないようにする。
fn constant_time_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_query_values() {
        let query = "url=https%3A%2F%2Fplay.google.com%2F%3Fid%3Dcom.a&token=abc";
        assert_eq!(query_value(query, "token").as_deref(), Some("abc"));
        assert_eq!(
            query_value(query, "url").as_deref(),
            Some("https://play.google.com/?id=com.a")
        );
        assert_eq!(query_value(query, "missing"), None);
    }

    #[test]
    fn reads_json_field() {
        assert_eq!(
            json_string_field(r#"{"url":"com.example.app"}"#, "url").as_deref(),
            Some("com.example.app")
        );
        assert_eq!(json_string_field("not json", "url"), None);
    }

    #[test]
    fn token_comparison() {
        assert!(constant_time_eq("abc", "abc"));
        assert!(!constant_time_eq("abc", "abd"));
        assert!(!constant_time_eq("abc", "abcd"));
    }
}
