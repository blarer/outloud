# Verify the overlay FOLLOWS the mouse: run overlay-demo (whose anchor is
# now the live cursor), park the pointer at two different spots, and
# screenshot each. If the card renders near both spots, following works; if
# it renders at one fixed place, it does not.
#
# SetCursorPos only moves the pointer; nothing is clicked.
$ErrorActionPreference = "Stop"
Add-Type -AssemblyName System.Drawing
Add-Type -AssemblyName System.Windows.Forms

$root = Split-Path -Parent $PSScriptRoot
$exe = Join-Path $root "target\debug\overlay-demo.exe"
if (-not (Test-Path $exe)) { throw "build first: cargo build -p overlay --bin overlay-demo --features display" }
$outDir = Join-Path $root "target\overlay-shots"
New-Item -ItemType Directory -Force -Path $outDir | Out-Null

function Shot([string]$name) {
    $b = [System.Windows.Forms.Screen]::PrimaryScreen.Bounds
    $bmp = New-Object System.Drawing.Bitmap($b.Width, $b.Height)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.CopyFromScreen($b.Location, [System.Drawing.Point]::Empty, $b.Size)
    $g.Dispose()
    $bmp.Save((Join-Path $outDir "$name.png"), [System.Drawing.Imaging.ImageFormat]::Png)
    $bmp.Dispose()
}

$p = Start-Process -FilePath $exe -PassThru
try {
    # Wait into the Listening state (step 3 of the script, ~6.5s in), then
    # sample two cursor positions a second apart. Both land inside visible
    # states (listening 5.0-7.5s, transcribing 7.5-10s).
    Start-Sleep -Milliseconds 5500
    [System.Windows.Forms.Cursor]::Position = New-Object System.Drawing.Point(300, 300)
    Start-Sleep -Milliseconds 1000
    Shot "follow-topleft"
    [System.Windows.Forms.Cursor]::Position = New-Object System.Drawing.Point(1100, 550)
    Start-Sleep -Milliseconds 1000
    Shot "follow-bottomright"
} finally {
    Stop-Process -Id $p.Id -Force -ErrorAction SilentlyContinue
}
Write-Host "shots: follow-topleft.png / follow-bottomright.png in $outDir"
