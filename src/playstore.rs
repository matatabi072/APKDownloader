//! Google Play のアプリ情報取得。
//!
//! Play には公開 API が無いので、通常のストアページを取って中身を読む。
//! ページには 2 種類の機械可読データが埋まっている。
//!
//! 1. `<script type="application/ld+json">` — 名前・開発者・アイコン・年齢区分など。
//!    構造化データとして意図的に置かれているので、比較的安定している。
//! 2. `AF_initDataCallback({key:'ds:5', ..., data:[...]})` — 画面描画用の生データ。
//!    バージョンや更新日時はここにしか無いが、**キー名が無くインデックス頼り**で、
//!    Google 側のレイアウト変更で位置が変わる。
//!
//! そのため 1 を土台にし、2 は「取れたら埋める」扱いにしている。取れなかった項目は
//! 適当な値で埋めず `None` のままにして、画面には「取得できず」と出す。
//! 推測値を本当の値のように見せる方が害が大きい。

use std::time::Duration;

use serde_json::Value;

/// Play が新レイアウトを返すよう、実在のブラウザーを名乗る。
/// 既定の reqwest UA だと簡略版 HTML が返り、ds ブロックごと消えることがある。
const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
                          (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";

pub fn build_client() -> reqwest::Result<reqwest::blocking::Client> {
    reqwest::blocking::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(Duration::from_secs(25))
        .connect_timeout(Duration::from_secs(10))
        .build()
}

/// ストアページから読み取ったアプリ情報。
///
/// 取れなかった項目は `None`。空文字で埋めない。
#[derive(Clone, Debug, Default)]
pub struct AppInfo {
    pub package: String,
    pub name: Option<String>,
    pub developer: Option<String>,
    pub icon_url: Option<String>,
    pub version: Option<String>,
    /// 「2026年7月30日」のような表示用の更新日。
    pub updated_text: Option<String>,
    /// 更新日時の UNIX 秒。並べ替えや差分表示に使う。
    pub updated_epoch: Option<i64>,
    pub installs: Option<String>,
    pub content_rating: Option<String>,
    pub genre: Option<String>,
    pub score: Option<f64>,
    pub price_text: Option<String>,
    pub android_version: Option<String>,
    pub description: Option<String>,
    /// 情報を取った国。どの地域の値かを画面に出すために持つ。
    pub region: String,
    /// 参照したページ。ブラウザーで開き直せるようにしておく。
    pub url: String,
}

impl AppInfo {
    pub fn title(&self) -> &str {
        self.name.as_deref().unwrap_or(&self.package)
    }

    /// ds ブロックから 1 つも読めなかったか。Play のレイアウト変更を検知して
    /// 画面に注意書きを出すために使う。
    ///
    /// バージョンは判定に入れない。Play はもともと多くのアプリで版を載せておらず、
    /// 「無い」ことが異常ではないため。
    pub fn detail_fields_missing(&self) -> bool {
        self.updated_text.is_none() && self.installs.is_none() && self.content_rating.is_none()
    }
}

/// ページ取得の生の結果。地域確認では本文より先にステータスを見たいので分けて返す。
pub struct PageResult {
    pub status: u16,
    pub body: String,
    pub url: String,
}

/// 入力からパッケージ ID を取り出す。
///
/// 受け付ける形:
/// - `https://play.google.com/store/apps/details?id=com.example.app&hl=ja`
/// - `market://details?id=com.example.app`
/// - `com.example.app` (ID 直接)
pub fn parse_package_id(input: &str) -> Option<String> {
    let input = input.trim();
    if input.is_empty() {
        return None;
    }

    // クエリに id= があればそれが最優先。URL 以外でも拾えるように単純検索にする。
    if let Some(rest) = input.split("id=").nth(1) {
        let id = rest.split(['&', '#', ' ', '"', '\'']).next().unwrap_or("");
        let id = urlencoding::decode(id).map(|s| s.into_owned()).unwrap_or_else(|_| id.to_string());
        if is_package_id(&id) {
            return Some(id);
        }
    }

    is_package_id(input).then(|| input.to_string())
}

/// Android のパッケージ ID として妥当か。
///
/// 正規の Java パッケージ名より緩めに見る。実際のストアには
/// 数字始まりのセグメントを含む ID も存在するため、厳密に弾くと取りこぼす。
pub fn is_package_id(s: &str) -> bool {
    if s.len() < 3 || s.len() > 255 || !s.contains('.') {
        return false;
    }
    s.split('.').all(|seg| {
        !seg.is_empty() && seg.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    })
}

pub fn store_url(package: &str, lang: &str, country: &str) -> String {
    format!(
        "https://play.google.com/store/apps/details?id={}&hl={}&gl={}",
        urlencoding::encode(package),
        urlencoding::encode(lang),
        urlencoding::encode(country),
    )
}

/// ストアページを取る。404 でもエラーにせず、状態として返す。
pub fn fetch_page(
    client: &reqwest::blocking::Client,
    package: &str,
    lang: &str,
    country: &str,
) -> anyhow::Result<PageResult> {
    let url = store_url(package, lang, country);
    let response = client.get(&url).send()?;
    let status = response.status().as_u16();
    // 404 の本文は読む意味が無いので捨てる。地域確認で 10 国叩くときの無駄を省く。
    let body = if status == 200 { response.text()? } else { String::new() };
    Ok(PageResult { status, body, url })
}

/// ページを取って解析するところまで。
pub fn fetch_info(
    client: &reqwest::blocking::Client,
    package: &str,
    lang: &str,
    country: &str,
) -> anyhow::Result<AppInfo> {
    let page = fetch_page(client, package, lang, country)?;
    match page.status {
        200 => {}
        404 => anyhow::bail!(
            "{country} のストアにこのアプリはありません (配信対象外・公開停止・ID 違いのいずれか)"
        ),
        other => anyhow::bail!("ストアページを取得できません (HTTP {other})"),
    }
    Ok(parse_info(&page.body, package, country, &page.url))
}

/// HTML からアプリ情報を組み立てる。
pub fn parse_info(html: &str, package: &str, country: &str, url: &str) -> AppInfo {
    let mut info = AppInfo {
        package: package.to_string(),
        region: country.to_string(),
        url: url.to_string(),
        ..Default::default()
    };

    // まず構造化データ。ここが取れれば主要項目は埋まる。
    if let Some(ld) = json_ld_software_application(html) {
        info.name = ld.get("name").and_then(as_clean_string);
        info.developer = ld.pointer("/author/name").and_then(as_clean_string);
        info.icon_url = ld.get("image").and_then(as_clean_string);
        info.description = ld.get("description").and_then(as_clean_string);
        info.content_rating = ld.get("contentRating").and_then(as_clean_string);
        info.genre = ld.get("applicationCategory").and_then(as_clean_string);
        info.score = ld.pointer("/aggregateRating/ratingValue").and_then(as_f64);
        info.android_version = ld.get("operatingSystem").and_then(as_clean_string);
        info.price_text = ld
            .pointer("/offers/0/price")
            .and_then(as_clean_string)
            .map(|p| if p == "0" { "無料".to_string() } else { p });
    }

    // 次に描画用データ。位置指定が外れても他の項目は生きるよう、1 項目ずつ独立に読む。
    if let Some(ds) = ds_blob(html, "ds:5") {
        merge_ds_fields(&mut info, &ds);
    }

    // ld+json が無いページ向けの最終手段。og: メタタグは Play でも残っている。
    if info.name.is_none() {
        info.name = meta_content(html, "og:title").map(|t| {
            // 「アプリ名 - Google Play のアプリ」から後半を落とす。
            t.split(" - ").next().unwrap_or(&t).trim().to_string()
        });
    }
    if info.icon_url.is_none() {
        info.icon_url = meta_content(html, "og:image");
    }
    if info.description.is_none() {
        info.description = meta_content(html, "og:description");
    }

    info
}

/// 描画用データから、キーの無いインデックス位置で値を拾う。
///
/// パスは google-play-scraper が使っているものに合わせてある。Google 側の変更で
/// 外れうるので、読めなければ黙って諦め、`None` のままにする。
fn merge_ds_fields(info: &mut AppInfo, ds: &Value) {
    /// `ds` の配列を添字で潜る。途中で型が違えば `None`。
    fn at<'a>(v: &'a Value, path: &[usize]) -> Option<&'a Value> {
        path.iter().try_fold(v, |cur, &i| cur.get(i))
    }

    if info.name.is_none() {
        info.name = at(ds, &[1, 2, 0, 0]).and_then(as_clean_string);
    }
    if info.developer.is_none() {
        info.developer = at(ds, &[1, 2, 68, 0]).and_then(as_clean_string);
    }
    if info.icon_url.is_none() {
        info.icon_url = at(ds, &[1, 2, 95, 0, 3, 2]).and_then(as_clean_string);
    }

    // バージョンは公開しているアプリとしていないアプリがある。実測 (2026-08) では
    // YouTube / VLC / WhatsApp / LINE のいずれも `[140][0][0]` が null で、
    // Play のページにアプリの版が載っていない。取れないのが普通だと思って扱う。
    //
    // 探索は `[140]` の部分木に限る。木全体を舐めると評価点の表示文字列 ("3.2" など)
    // をバージョンと誤認する — 実際にそうなっていた。
    info.version = at(ds, &[1, 2, 140, 0, 0, 0])
        .and_then(as_clean_string)
        .or_else(|| at(ds, &[1, 2, 140, 0]).and_then(|sub| find_version(sub, 0)));

    // 対応 Android は `[140][1][0][0]` に `[API レベル, "表示用の版"]` の形で入る。
    // 表示用の文字列が無いこともあるので、その場合は API レベルを出す。
    if let Some(entry) = at(ds, &[1, 2, 140, 1, 0, 0])
        && let Some(text) = as_clean_string(&entry[1])
            .map(|version| format!("Android {version} 以上"))
            .or_else(|| entry[0].as_i64().map(|api| format!("API レベル {api} 以上")))
    {
        // ld+json の operatingSystem は "Android" としか言わないので、こちらを優先する。
        info.android_version = Some(text);
    }

    info.updated_text = at(ds, &[1, 2, 145, 0, 0]).and_then(as_clean_string);
    info.updated_epoch = at(ds, &[1, 2, 145, 0, 1, 0]).and_then(Value::as_i64);

    if info.installs.is_none() {
        info.installs = at(ds, &[1, 2, 13, 0]).and_then(as_clean_string);
    }
    if info.content_rating.is_none() {
        info.content_rating = at(ds, &[1, 2, 9, 0]).and_then(as_clean_string);
    }
    if info.genre.is_none() {
        info.genre = at(ds, &[1, 2, 79, 0, 0, 0]).and_then(as_clean_string);
    }
    if info.score.is_none() {
        info.score = at(ds, &[1, 2, 51, 0, 1]).and_then(as_f64);
    }
    if info.price_text.is_none() {
        info.price_text = at(ds, &[1, 2, 57, 0, 0, 0, 0, 1, 0, 2]).and_then(as_clean_string);
    }
}

/// バージョン欄の部分木から、番号の形をした文字列を拾う。
///
/// **必ずバージョン欄の部分木だけに当てること。** 木全体に当てると、評価点や
/// 日付など「数字とドット」でできた別の値を拾ってしまう。
fn find_version(v: &Value, depth: usize) -> Option<String> {
    if depth > 6 {
        return None;
    }
    match v {
        Value::String(s) => looks_like_version(s).then(|| s.clone()),
        Value::Array(items) => items.iter().find_map(|item| find_version(item, depth + 1)),
        _ => None,
    }
}

/// `1.2` / `10.4.1` / `2026.07.30-release` のような形か。
fn looks_like_version(s: &str) -> bool {
    if s.len() > 32 || s.is_empty() {
        return false;
    }
    // 先頭は数字、ドットを 1 つ以上含み、数字とドットで始まる部分が続くこと。
    let head: String = s.chars().take_while(|c| c.is_ascii_digit() || *c == '.').collect();
    head.len() >= 3 && head.contains('.') && head.starts_with(|c: char| c.is_ascii_digit())
}

/// `<script type="application/ld+json">` から SoftwareApplication を取り出す。
fn json_ld_software_application(html: &str) -> Option<Value> {
    let mut rest = html;
    while let Some(start) = rest.find("application/ld+json") {
        rest = &rest[start..];
        // 属性の並び順に依存しないよう、タグの閉じ `>` を探してから本文に入る。
        let Some(open_end) = rest.find('>') else { break };
        let after = &rest[open_end + 1..];
        let Some(close) = after.find("</script>") else { break };
        let body = after[..close].trim();
        rest = &after[close..];

        if let Ok(value) = serde_json::from_str::<Value>(body) {
            // ページには BreadcrumbList など別の ld+json も入っている。型で選ぶ。
            for candidate in flatten_ld(value) {
                if candidate.get("@type").and_then(Value::as_str) == Some("SoftwareApplication") {
                    return Some(candidate);
                }
            }
        }
    }
    None
}

/// ld+json は単体・配列・`@graph` のどれでも来るので平らにする。
fn flatten_ld(value: Value) -> Vec<Value> {
    match value {
        Value::Array(items) => items.into_iter().flat_map(flatten_ld).collect(),
        Value::Object(ref map) if map.contains_key("@graph") => {
            let graph = map.get("@graph").cloned().unwrap_or(Value::Null);
            flatten_ld(graph)
        }
        other => vec![other],
    }
}

/// `AF_initDataCallback({key:'ds:5', hash:'6', data:[...], sideChannel:{}})` の
/// `data` 配列だけを取り出して JSON として解析する。
///
/// 呼び出し全体は JS のオブジェクトリテラル (キーが無引用符・値が単引用符) なので
/// そのままでは JSON として読めない。`data:` の後ろの配列だけは純粋な JSON なので、
/// 括弧の対応を数えてその範囲を切り出す。
pub fn ds_blob(html: &str, key: &str) -> Option<Value> {
    // ページには同じキー名が 2 通りで現れる。
    //   1. `'ds:5' : {id:'Ws7gDc', request:[...]}`  ← 読み込み要求の対応表。中身は無い
    //   2. `AF_initDataCallback({key: 'ds:5', ..., data:[...]})` ← 欲しいのはこちら
    // 単に最初の出現を採ると 1 を掴み、そこから後方の `data:` を探して**別の**
    // ブロックを読んでしまう。直前が `key:` であることを条件に 2 だけを選ぶ。
    const KEY_TO_DATA_SPAN: usize = 200;

    for quote in ['\'', '"'] {
        let needle = format!("{quote}{key}{quote}");
        let mut from = 0usize;

        while let Some(offset) = html[from..].find(&needle) {
            let pos = from + offset;
            from = pos + needle.len();

            if !html[..pos].trim_end().ends_with("key:") {
                continue;
            }

            // `data:` は同じ呼び出しの中にある。離れていたら別ブロックなので見送る。
            let rest = &html[from..];
            let Some(data_pos) = rest.find("data:") else { continue };
            if data_pos > KEY_TO_DATA_SPAN {
                continue;
            }

            let after_data = &rest[data_pos..];
            let Some(open) = after_data.find('[') else { continue };
            if let Some(slice) = balanced_slice(&after_data[open..])
                && let Ok(value) = serde_json::from_str(slice)
            {
                return Some(value);
            }
        }
    }
    None
}

/// 先頭の `[` から対応する `]` までを返す。文字列リテラルの中の括弧は数えない。
fn balanced_slice(s: &str) -> Option<&str> {
    let bytes = s.as_bytes();
    let mut depth = 0i32;
    let mut in_string = false;
    let mut escaped = false;

    for (i, &b) in bytes.iter().enumerate() {
        if in_string {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_string = false;
            }
            continue;
        }
        match b {
            b'"' => in_string = true,
            b'[' => depth += 1,
            b']' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&s[..=i]);
                }
            }
            _ => {}
        }
    }
    None
}

/// `<meta property="og:title" content="...">` の中身。
fn meta_content(html: &str, property: &str) -> Option<String> {
    let needle = format!("property=\"{property}\"");
    let pos = html.find(&needle)?;
    // meta タグは content が前後どちらにも書かれうるので、タグ全体を切り出して探す。
    let tag_start = html[..pos].rfind('<')?;
    let tag_end = html[pos..].find('>')? + pos;
    let tag = &html[tag_start..tag_end];

    let content_pos = tag.find("content=\"")? + "content=\"".len();
    let rest = &tag[content_pos..];
    let end = rest.find('"')?;
    let raw = &rest[..end];
    Some(decode_entities(raw)).filter(|s| !s.trim().is_empty())
}

/// HTML 実体参照のうち、Play のメタタグに実際に出てくるものだけ戻す。
fn decode_entities(s: &str) -> String {
    s.replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
        .trim()
        .to_string()
}

fn as_clean_string(v: &Value) -> Option<String> {
    let s = v.as_str()?.trim();
    (!s.is_empty()).then(|| decode_entities(s))
}

fn as_f64(v: &Value) -> Option<f64> {
    v.as_f64().or_else(|| v.as_str()?.parse().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_package_from_various_inputs() {
        let cases = [
            "https://play.google.com/store/apps/details?id=com.example.app&hl=ja",
            "https://play.google.com/store/apps/details?hl=ja&id=com.example.app",
            "market://details?id=com.example.app",
            "com.example.app",
        ];
        for case in cases {
            assert_eq!(parse_package_id(case).as_deref(), Some("com.example.app"), "{case}");
        }
    }

    #[test]
    fn rejects_non_packages() {
        for case in ["", "https://example.com/", "notapackage", "com"] {
            assert_eq!(parse_package_id(case), None, "{case}");
        }
    }

    #[test]
    fn extracts_balanced_data_array() {
        let html = r#"<script>AF_initDataCallback({key: 'ds:5', hash: '6', data:[1,["a]b",[2,3]]], sideChannel: {}});</script>"#;
        let value = ds_blob(html, "ds:5").expect("ds:5 を読めること");
        assert_eq!(value[1][0], "a]b");
        assert_eq!(value[1][1][1], 3);
    }

    #[test]
    fn version_shape_check() {
        assert!(looks_like_version("1.2"));
        assert!(looks_like_version("10.4.1-beta"));
        assert!(!looks_like_version("2026"));
        assert!(!looks_like_version("hello"));
    }

    /// 実際の Play ページに当てる確認。
    ///
    /// ds ブロックの位置指定は Google 側のレイアウト変更で外れる。ここが落ちたら
    /// 「バージョンと更新日が取れなくなった」の合図なので、`merge_ds_fields` の
    /// パスを見直す。通信するので既定では走らせない:
    /// `cargo test -- --ignored --nocapture`
    #[test]
    #[ignore = "ネットワークに出るため既定では走らせない"]
    fn fetches_a_real_app_page() {
        let client = build_client().expect("HTTP クライアント");
        let info = fetch_info(&client, "com.google.android.youtube", "ja", "JP")
            .expect("YouTube のページを取得できること");

        println!("{info:#?}");
        assert!(info.name.is_some(), "アプリ名が取れていない");
        assert!(info.developer.is_some(), "開発者が取れていない");
        assert!(info.icon_url.is_some(), "アイコン URL が取れていない");
        assert!(
            !info.detail_fields_missing(),
            "ds ブロックから 1 項目も読めていない。merge_ds_fields のパスを見直すこと"
        );
        assert!(info.updated_epoch.is_some(), "更新日時が取れていない");
        // 評価点の表示文字列 ("3.2") をバージョンと取り違えていた回帰を防ぐ。
        // Play が版を出していないアプリでは None が正しい。
        if let Some(version) = &info.version {
            let rating = info.score.map(|s| format!("{s:.1}"));
            assert_ne!(Some(version.as_str()), rating.as_deref(), "評価点をバージョンと誤認している");
        }
    }

    /// ds ブロックの該当箇所を実物で見るための調査用。
    ///
    /// `merge_ds_fields` の添字は Google 側のレイアウト変更で動く。値が取れなくなったら
    /// 推測で添字をいじる前にこれを走らせ、実際の構造を見てから直すこと。
    /// `cargo test dump_ds_layout -- --ignored --nocapture`
    #[test]
    #[ignore = "調査用"]
    fn dump_ds_layout() {
        let client = build_client().expect("HTTP クライアント");
        for package in
            ["com.google.android.youtube", "org.videolan.vlc", "jp.naver.line.android"]
        {
            let Ok(page) = fetch_page(&client, package, "ja", "JP") else { continue };
            if page.status != 200 {
                println!("{package}: HTTP {}", page.status);
                continue;
            }
            let Some(ds) = ds_blob(&page.body, "ds:5") else {
                println!("{package}: ds:5 が見つからない");
                continue;
            };
            // 添字がずれたときに一番影響が大きい枝を出す。
            for (label, path) in [("[140] 対応 Android/版", 140usize), ("[145] 更新日", 145)] {
                println!(
                    "{package} {label} = {}",
                    serde_json::to_string(&ds[1][2][path]).unwrap_or_default()
                );
            }
            let info = parse_info(&page.body, package, "JP", &page.url);
            println!(
                "  version={:?} android={:?} updated={:?}",
                info.version, info.android_version, info.updated_text
            );
        }
    }

    /// 配信対象外の国を 404 として扱えるか。存在しないパッケージで代用する。
    #[test]
    #[ignore = "ネットワークに出るため既定では走らせない"]
    fn missing_package_is_not_listed() {
        let client = build_client().expect("HTTP クライアント");
        let page = fetch_page(&client, "com.example.definitely.not.real.app", "en", "US")
            .expect("通信自体は成功すること");
        assert_eq!(page.status, 404, "存在しない ID は 404 になるはず");
    }

    #[test]
    fn reads_json_ld_software_application() {
        let html = r#"
          <script type="application/ld+json">{"@type":"BreadcrumbList"}</script>
          <script type="application/ld+json" nonce="x">
            {"@type":"SoftwareApplication","name":"Test &amp; App",
             "author":{"name":"Acme"},"image":"https://x/icon.png"}
          </script>"#;
        let info = parse_info(html, "com.example.app", "JP", "https://example.invalid");
        assert_eq!(info.name.as_deref(), Some("Test & App"));
        assert_eq!(info.developer.as_deref(), Some("Acme"));
        assert_eq!(info.icon_url.as_deref(), Some("https://x/icon.png"));
    }
}
