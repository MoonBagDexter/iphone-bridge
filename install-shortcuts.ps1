# Creates/refreshes the Desktop and Startup shortcuts for iphone-bridge.
# Idempotent — run after every rebuild.
$repo = $PSScriptRoot
# Build artifacts live on D: (see .cargo/config.toml) because C: is nearly full
$exe = 'D:\cargo-builds\iphone-bridge\release\iphone-bridge.exe'
if (-not (Test-Path $exe)) { Write-Error "Build first: cargo build --release ($exe not found)"; exit 1 }

$targets = @(
    (Join-Path ([Environment]::GetFolderPath('Desktop')) 'iPhone Bridge.lnk'),
    (Join-Path ([Environment]::GetFolderPath('Startup')) 'iPhone Bridge.lnk')
)

$shell = New-Object -ComObject WScript.Shell
foreach ($lnkPath in $targets) {
    $lnk = $shell.CreateShortcut($lnkPath)
    $lnk.TargetPath = $exe
    $lnk.WorkingDirectory = $repo
    $lnk.Description = 'iPhone as Windows headset over Wi-Fi'
    $lnk.Save()
    Write-Host "Created $lnkPath"
}
