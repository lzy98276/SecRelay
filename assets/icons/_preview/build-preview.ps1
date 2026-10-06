# 生成「仿 SecRandom」这一轮的预览页：参考图 + 三个强调色变体，内联渲染后由无头 Edge 截图。
$ErrorActionPreference = 'Stop'

$root = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
$outDir = Join-Path $root '_preview'
New-Item -ItemType Directory -Force -Path $outDir | Out-Null

# 参考图转 data URI —— 无头模式对 file:// 子资源有限制，内联才稳
$refBytes = [System.IO.File]::ReadAllBytes((Join-Path $outDir 'ref-secrandom.png'))
$refUri = 'data:image/png;base64,' + [System.Convert]::ToBase64String($refBytes)

$variants = @(
  @{ File = 'concept-p4-plane.svg'; Title = 'P4 · 琥珀强调'; Note = '强调色 #F2A33C。蓝主色 + 一个非蓝点睛，和 SecRandom 的结构完全一致' },
  @{ File = 'concept-p5-jade.svg';  Title = 'P5 · 墨绿强调'; Note = '强调色 #36AA75，就是 SecRandom 本人那个色。家族一致度最高' },
  @{ File = 'concept-p6-cyan.svg';  Title = 'P6 · 青蓝强调'; Note = '强调色 #22D3EE。完全守蓝色家族，代价是点睛不跳' }
)

$rows = ''
foreach ($v in $variants) {
  $svg = (Get-Content (Join-Path $root $v.File) -Raw) -replace '\s+width="256"\s+height="256"', ''
  $plain = ''
  foreach ($size in @(48, 32, 16)) {
    $plain += "<figure><div class=`"icon s$size`">$svg</div><figcaption>$size</figcaption></figure>"
  }
  $rows += @"
    <div class="row">
      <div class="label"><b>$($v.Title)</b><span>$($v.Note)</span></div>
      <div class="tile white"><div class="icon s128">$svg</div></div>
      <div class="tile light">$plain</div>
      <div class="tile dark">$plain</div>
    </div>
"@
}

# 参考图行（原图是满幅白底方形，放在浅灰格上才能看出边界）
$refSizes = ''
foreach ($size in @(48, 32, 16)) {
  $refSizes += "<figure><div class=`"icon s$size`"><img src=`"$refUri`" style=`"width:100%;height:100%;display:block`"></div><figcaption>$size</figcaption></figure>"
}
$refRow = @"
    <div class="row">
      <div class="label"><b>参考 · SecRandom</b><span>原图。满幅白底、两个叠圆、宽肩形、溢出主体的对勾</span></div>
      <div class="tile white"><div class="icon s128"><img src="$refUri" style="width:100%;height:100%;display:block"></div></div>
      <div class="tile light">$refSizes</div>
      <div class="tile dark">$refSizes</div>
    </div>
"@

$html = @"
<!doctype html>
<html lang="zh-CN"><head><meta charset="utf-8"><title>SecRelay 图标 · 仿 SecRandom</title>
<style>
  body { margin: 0; padding: 22px 26px 28px; background: #0B0C0F;
         font-family: "Microsoft YaHei", system-ui, sans-serif; color: #F2F4F8; }
  h1 { font-size: 17px; font-weight: 600; margin: 0 0 4px; }
  .sub { font-size: 12px; color: #71767F; margin-bottom: 16px; }
  .row { display: flex; align-items: center; gap: 16px; padding: 12px 16px;
         border: 1px solid #22252C; border-radius: 14px; margin-bottom: 12px; background: #101116; }
  .label { width: 168px; flex: 0 0 168px; }
  .label b { display: block; font-size: 14px; font-weight: 600; }
  .label span { font-size: 11px; color: #71767F; line-height: 1.45; display: block; margin-top: 3px; }
  .tile { display: flex; align-items: flex-end; gap: 14px; padding: 8px 12px; border-radius: 10px; }
  .tile.white { background: #FFFFFF; }
  .tile.light { background: #EEF1F6; }
  .tile.dark  { background: #1B1D23; }
  figure { margin: 0; text-align: center; }
  figcaption { font-size: 10px; color: #8A9099; margin-top: 5px; }
  .dark figcaption { color: #6A7078; }
  .icon svg { width: 100%; height: 100%; display: block; }
  .s128 { width: 128px; height: 128px; }
  .s48 { width: 48px; height: 48px; }
  .s32 { width: 32px; height: 32px; }
  .s16 { width: 16px; height: 16px; }
</style></head>
<body>
  <h1>SecRelay 图标 · 仿 SecRandom 语汇（修正体量）</h1>
  <div class="sub">纯白满幅底 · 主蓝 #127AD2 · 浅蓝 #78BAE6 · 三组底色依次为白 / 浅灰 / 深色</div>
$refRow
$rows
</body></html>
"@

$htmlPath = Join-Path $outDir 'concepts-p456.html'
Set-Content -Path $htmlPath -Value $html -Encoding UTF8

$edge = 'C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe'
$png = Join-Path $outDir 'concepts-p456.png'
& $edge --headless=new --disable-gpu --hide-scrollbars --force-device-scale-factor=1 `
        --window-size=1180,760 --screenshot=$png "file:///$($htmlPath -replace '\\','/')" | Out-Null

Write-Host "html: $htmlPath"
Write-Host "png : $png ($((Get-Item $png).Length) bytes)"




