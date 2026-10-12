# Rebuild PhotoCraft from this checkout and copy it to the install folder.
# Usage: .\install.ps1 [-Dest C:\Apps\Photocraft]
param(
    [string]$Dest = 'C:\Apps\Photocraft'
)
$ErrorActionPreference = 'Stop'
Set-Location $PSScriptRoot

cargo build --release -p photocraft
if ($LASTEXITCODE -ne 0) { throw "Build failed; the installed copy in $Dest was left untouched." }

if (Get-Process photocraft -ErrorAction SilentlyContinue | Where-Object { $_.Path -eq (Join-Path $Dest 'photocraft.exe') }) {
    throw "PhotoCraft is running from $Dest. Close it and run this script again."
}

New-Item -ItemType Directory -Force $Dest | Out-Null
Copy-Item target\release\photocraft.exe (Join-Path $Dest 'photocraft.exe') -Force
Write-Host "Installed to $Dest"
