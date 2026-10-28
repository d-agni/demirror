# Fails if a program or the driver in $Folder needs a DLL that Windows doesn't come with
# (the Visual C++ runtime, say): demirror must run on a fresh Windows.
param([Parameter(Mandatory)][string]$Folder)
$ErrorActionPreference = "Stop"

$vswhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
$dumpbin = & $vswhere -latest -products * -find "VC\Tools\MSVC\**\bin\Hostx64\x64\dumpbin.exe" | Select-Object -First 1
if (-not $dumpbin) { throw "dumpbin not found" }

$missing = @()
foreach ($file in Get-ChildItem -Recurse $Folder -Include *.exe, *.dll) {
    $dlls = & $dumpbin /nologo /dependents $file.FullName |
        Where-Object { $_ -match '^\s+\S+\.dll\s*$' } | ForEach-Object { $_.Trim() }
    Write-Host "$($file.Name): $($dlls -join ', ')"
    foreach ($dll in $dlls) {
        $system = (Test-Path "$env:SystemRoot\System32\$dll") -or ($dll -match '^(api|ext)-ms-win-')
        if ($dll -match '^(vcruntime|msvcp|concrt|ucrtbased|vccorlib)' -or -not $system) {
            $missing += "$($file.Name) needs $dll"
        }
    }
}
if ($missing) { throw ($missing -join "`n") }
Write-Host "Everything needed comes with Windows."
