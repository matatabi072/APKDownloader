// Google Play のページから、常駐中の APK Downloader へ URL を渡す。
//
// ブラウザーごとの差はここで吸収する。Chrome/Edge は `chrome`、Firefox は `browser`
// を提供するが、Firefox は `chrome` も別名として持っているので `chrome` に寄せる。
// 例外は「Promise を返すか、コールバックを取るか」で、そこは await で包んで揃える。

const MENU_ID = "apkd-fetch";
const DEFAULTS = { port: 48915, token: "" };

// Play のアプリページだけを対象にする。検索結果や開発者ページで出しても送れない。
const APP_PAGE_PATTERNS = [
  "*://play.google.com/store/apps/details*",
  "*://play.google.com/work/apps/details*",
];

chrome.runtime.onInstalled.addListener(() => {
  // 再インストール時に二重登録されないよう、作る前に必ず消す。
  chrome.contextMenus.removeAll(() => {
    chrome.contextMenus.create({
      id: MENU_ID,
      title: "APK Downloader で取得",
      // ページのどこを右クリックしても出す。
      // `["page", "link", "selection"]` にしていたときは、アプリのアイコンや
      // スクリーンショットの上で右クリックすると画像用のメニューになり、項目が
      // 消えていた (Firefox 153 で確認)。Play のページは大半が画像なので実害が大きい。
      contexts: ["all"],
      // 出す場所は「Play のアプリページを見ているとき」だけに絞る。
      documentUrlPatterns: APP_PAGE_PATTERNS,
      // `targetUrlPatterns` は付けない。付けるとリンクや画像の上では
      // **その要素の URL**が照合対象になる。Play のアイコンは
      // googleusercontent.com から配られるので決して一致せず、やはり項目が消える。
    });
  });
});

chrome.contextMenus.onClicked.addListener(async (info) => {
  if (info.menuItemId !== MENU_ID) return;

  // 近いものから順に見て、**パッケージ ID を取り出せた最初のもの**を使う。
  // 単に `linkUrl || pageUrl` とすると、Play のロゴなどアプリページ以外へのリンクを
  // 右クリックしたときに、そのリンクを掴んで「送れませんでした」で終わってしまう。
  // 取り出せなければ次の候補へ落ちるようにして、最後はページ自身に行き着かせる。
  let packageId = null;
  for (const candidate of [info.linkUrl, info.selectionText, info.pageUrl]) {
    packageId = extractPackageId(candidate);
    if (packageId) break;
  }

  if (!packageId) {
    notify("送れませんでした", "Google Play のアプリページの URL が見つかりません。");
    return;
  }

  await send(packageId);
});

/** 設定を読む。未設定なら既定値で埋める。 */
async function loadSettings() {
  const stored = await chrome.storage.local.get(DEFAULTS);
  return { port: Number(stored.port) || DEFAULTS.port, token: stored.token || "" };
}

/** URL からパッケージ ID を取り出す。アプリ側と同じ規則にそろえる。 */
function extractPackageId(text) {
  if (!text) return null;
  const match = /[?&]id=([A-Za-z0-9_.]+)/.exec(text);
  if (match) return match[1];
  // ID そのものが選択されている場合。ドットを含む英数字だけを受け付ける。
  const trimmed = text.trim();
  if (/^[A-Za-z0-9_]+(\.[A-Za-z0-9_]+)+$/.test(trimmed)) return trimmed;
  return null;
}

/** 常駐アプリへ渡す。 */
async function send(packageId) {
  const { port, token } = await loadSettings();

  if (!token) {
    notify(
      "合言葉が未設定です",
      "拡張機能の設定画面を開き、APK Downloader の設定にある合言葉を貼り付けてください。"
    );
    chrome.runtime.openOptionsPage();
    return;
  }

  try {
    const response = await fetch(`http://127.0.0.1:${port}/enqueue`, {
      method: "POST",
      headers: { "Content-Type": "application/json", "X-APKD-Token": token },
      body: JSON.stringify({ url: packageId }),
    });

    const result = await response.json().catch(() => ({}));

    if (response.status === 403) {
      notify("合言葉が違います", "設定画面で APK Downloader 側の合言葉を貼り直してください。");
      return;
    }
    if (!response.ok || !result.ok) {
      notify("受け付けられませんでした", result.error || `HTTP ${response.status}`);
      return;
    }
    notify("取得を開始しました", packageId);
  } catch (e) {
    // 到達できない = 常駐アプリが起動していない、かポート違い。
    notify(
      "APK Downloader に接続できません",
      `127.0.0.1:${port} に届きません。アプリが起動しているか、ポート番号が合っているか確認してください。`
    );
  }
}

function notify(title, message) {
  chrome.notifications.create({
    type: "basic",
    iconUrl: chrome.runtime.getURL("icons/icon128.png"),
    title,
    message,
  });
}
