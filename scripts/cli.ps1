# Runs the developer CLI with GStreamer on PATH (Windows PowerShell).
# Example: .\scripts\cli.ps1 stream --room TESTXX --name Voce
$gst = "C:\Program Files\gstreamer\1.0\msvc_x86_64\bin"
if (-not (Test-Path $gst)) { Write-Error "GStreamer not found at $gst"; exit 1 }
$env:PATH = "$gst;$env:PATH"
$exe = Join-Path $PSScriptRoot "..\target\debug\screenlive-cli.exe"
if (-not (Test-Path $exe)) { Write-Error "CLI not built yet: run 'cargo build -p screenlive-core'"; exit 1 }
& $exe @args
exit $LASTEXITCODE
