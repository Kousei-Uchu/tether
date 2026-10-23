# Packages the Electron app on Windows: an NSIS installer and a .zip in apps\desktop\dist\.
# Needs Rust, the Visual Studio C++ build tools and Node 22+.
$ErrorActionPreference = 'Stop'
Set-Location (Join-Path $PSScriptRoot '..\apps\desktop')
npm ci
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
npm run dist
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
Write-Host 'Done. See apps\desktop\dist\'
