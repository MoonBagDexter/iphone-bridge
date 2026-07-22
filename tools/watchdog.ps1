# Restarts the iPhone bridge if it is not running.
# Run on a timer by the "iphone-bridge-watchdog" scheduled task.
# The Run registry key only launches the bridge at logon; it does nothing if the
# process later exits (a crash, or a rebuild that killed it and never relaunched).

$exe = Join-Path $PSScriptRoot '..\target\x86_64-pc-windows-gnu\release\iphone-bridge.exe'
$exe = [System.IO.Path]::GetFullPath($exe)

if (-not (Test-Path $exe)) {
    exit 1
}

if (Get-Process iphone-bridge -ErrorAction SilentlyContinue) {
    exit 0
}

Start-Process $exe
