const DEFAULTS = { port: 48915, token: "" };

const portInput = document.getElementById("port");
const tokenInput = document.getElementById("token");
const statusLine = document.getElementById("status");

init();

async function init() {
  const stored = await chrome.storage.local.get(DEFAULTS);
  portInput.value = stored.port || DEFAULTS.port;
  tokenInput.value = stored.token || "";
}

document.getElementById("save").addEventListener("click", async () => {
  const port = Number(portInput.value);
  if (!Number.isInteger(port) || port < 1 || port > 65535) {
    show("ポート番号が正しくありません。", false);
    return;
  }
  await chrome.storage.local.set({ port, token: tokenInput.value.trim() });
  show("保存しました。", true);
});

document.getElementById("test").addEventListener("click", async () => {
  const port = Number(portInput.value) || DEFAULTS.port;
  show("確認しています…", true);
  try {
    // /ping は合言葉なしで答える口。到達性だけをここで確かめる。
    const response = await fetch(`http://127.0.0.1:${port}/ping`, { cache: "no-store" });
    const result = await response.json();
    if (result.ok) {
      show(`つながりました（${result.app} v${result.version}${result.debug ? " / DEBUG" : ""}）。`, true);
    } else {
      show("応答はありましたが、APK Downloader ではないようです。", false);
    }
  } catch (e) {
    show(`127.0.0.1:${port} に届きません。アプリが起動しているか確認してください。`, false);
  }
});

function show(message, ok) {
  statusLine.textContent = message;
  statusLine.className = ok ? "ok" : "ng";
}
