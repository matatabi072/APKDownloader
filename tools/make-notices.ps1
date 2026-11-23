#requires -Version 5.1
<#
.SYNOPSIS
  THIRD-PARTY-NOTICES.md を生成する。

.DESCRIPTION
  Rust は依存を静的リンクするので、MIT / BSD / ISC などの「著作権表示を残す」義務が
  実行ファイル側に乗る。実際にリンクされるクレートを cargo metadata から出し、
  各クレートに同梱されている LICENSE 本文を集めて 1 つのファイルにまとめる。

  --filter-platform を付けるのは、他プラットフォーム専用の依存 (Linux の x11 等) を
  数えないため。付けないと、実際には含まれないクレートまで並ぶ。

.NOTES
  このスクリプト自身は **UTF-8 BOM 付き** で保存すること。
  BOM が無いと PowerShell 5.1 が ANSI として読み、日本語が壊れる。
#>
param(
    [string]$Target = "x86_64-pc-windows-msvc",
    [string]$OutFile = (Join-Path $PSScriptRoot "..\THIRD-PARTY-NOTICES.md")
)

$ErrorActionPreference = "Stop"
$root = Resolve-Path (Join-Path $PSScriptRoot "..")
Push-Location $root
try {
    Write-Host "cargo metadata を取得しています ($Target)..."
    $json = cargo metadata --format-version 1 --filter-platform $Target 2>$null
    if (-not $json) { throw "cargo metadata に失敗しました" }
    $meta = $json | ConvertFrom-Json

    # 自分のクレートは除く
    $ownNames = @($meta.workspace_members | ForEach-Object { ($_ -split '[ @]')[0] })

    $packages = $meta.packages |
        Where-Object { $ownNames -notcontains $_.name } |
        Sort-Object name, version -Unique

    Write-Host ("対象クレート: {0} 件" -f @($packages).Count)

    $sb = New-Object System.Text.StringBuilder
    [void]$sb.AppendLine("# サードパーティ ライセンス表示")
    [void]$sb.AppendLine()
    [void]$sb.AppendLine("本ソフトウェアは以下のオープンソース ソフトウェアを利用しています。")
    [void]$sb.AppendLine("各ライセンスの全文を後半に収録しています。")
    [void]$sb.AppendLine()
    [void]$sb.AppendLine(("生成日: {0} / 対象: ``{1}``" -f (Get-Date -Format "yyyy-MM-dd"), $Target))
    [void]$sb.AppendLine()
    [void]$sb.AppendLine("## 一覧")
    [void]$sb.AppendLine()
    [void]$sb.AppendLine("| クレート | バージョン | ライセンス |")
    [void]$sb.AppendLine("|---|---|---|")
    foreach ($p in $packages) {
        $lic = if ($p.license) { $p.license } else { "(ファイル同梱)" }
        [void]$sb.AppendLine(("| {0} | {1} | {2} |" -f $p.name, $p.version, $lic))
    }

    # 同じ本文を何度も載せない (SiteLimitTimer では 226 クレートで本文 129 種だった)
    $texts = [ordered]@{}
    foreach ($p in $packages) {
        $dir = Split-Path $p.manifest_path -Parent
        $files = Get-ChildItem -LiteralPath $dir -File -ErrorAction SilentlyContinue |
            Where-Object { $_.Name -match '^(LICENSE|LICENCE|COPYING|NOTICE)' }
        foreach ($f in $files) {
            $body = (Get-Content -LiteralPath $f.FullName -Raw -ErrorAction SilentlyContinue)
            if (-not $body) { continue }
            $key = $body.Trim()
            if (-not $texts.Contains($key)) { $texts[$key] = New-Object System.Collections.ArrayList }
            [void]$texts[$key].Add(("{0} {1} ({2})" -f $p.name, $p.version, $f.Name))
        }
    }

    [void]$sb.AppendLine()
    [void]$sb.AppendLine(("## ライセンス本文 ({0} 種)" -f $texts.Count))
    $n = 0
    foreach ($key in $texts.Keys) {
        $n++
        [void]$sb.AppendLine()
        [void]$sb.AppendLine(("### {0}." -f $n))
        [void]$sb.AppendLine()
        [void]$sb.AppendLine("適用: " + (($texts[$key] | Sort-Object -Unique) -join ", "))
        [void]$sb.AppendLine()
        [void]$sb.AppendLine('```text')
        [void]$sb.AppendLine($key)
        [void]$sb.AppendLine('```')
    }

    $utf8 = New-Object System.Text.UTF8Encoding($false)
    [System.IO.File]::WriteAllText((Resolve-Path -LiteralPath (Split-Path $OutFile -Parent)).Path + "\" + (Split-Path $OutFile -Leaf), $sb.ToString(), $utf8)
    Write-Host ("書き出しました: {0} (本文 {1} 種)" -f $OutFile, $texts.Count)
}
finally {
    Pop-Location
}
