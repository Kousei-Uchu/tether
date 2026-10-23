# Builds tether.exe in release mode and copies it to dist\.
$ErrorActionPreference = 'Stop'
Set-Location (Join-Path $PSScriptRoot '..')
cargo build --release -p tether-cli
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
New-Item -ItemType Directory -Force dist | Out-Null
Copy-Item target\release\tether.exe dist\tether-windows-x86_64.exe
Write-Host 'Done. dist\tether-windows-x86_64.exe'
