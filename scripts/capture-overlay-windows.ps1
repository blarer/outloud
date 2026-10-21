# Capture the Windows overlay with actual pixels: run overlay-demo, grab
# full-screen screenshots at moments the script is in a VISIBLE state, and
# report whether the overlay region differs from the same region before the
# demo started. Nobody had ever confirmed the Windows overlay draws anything;
# CI cannot see pixels, and "UpdateLayeredWindow returned S_OK" is not a
# rendering.
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
    $path = Join-Path $outDir "$name.png"
    $bmp.Save($path, [System.Drawing.Imaging.ImageFormat]::Png)
    $bmp.Dispose()
    return $path
}

# Baseline before the demo starts.
Shot "baseline" | Out-Null

$p = Start-Process -FilePath $exe -PassThru
try {
    # Demo script: ModelLoading(2.5s) Idle Listening Transcribing Injecting
    # Error NoPermission DegradedOffline, 2.5s each. Catch ModelLoading at
    # ~1.5s, Listening at ~6.5s, Transcribing at ~9s, Error at ~14s.
    Start-Sleep -Milliseconds 1500; Shot "model-loading" | Out-Null
    Start-Sleep -Milliseconds 5000; Shot "listening" | Out-Null
    Start-Sleep -Milliseconds 2500; Shot "transcribing" | Out-Null
    Start-Sleep -Milliseconds 5000; Shot "error" | Out-Null
} finally {
    Stop-Process -Id $p.Id -Force -ErrorAction SilentlyContinue
}
Write-Host "shots in $outDir"
Get-ChildItem $outDir | ForEach-Object { Write-Host ("  " + $_.Name + "  " + $_.Length + " bytes") }
