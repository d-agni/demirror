# Signs demirror's programs and installer with the code-signing certificate in
# $env:DEMIRROR_SIGN_PFX (a base64 .pfx) and $env:DEMIRROR_SIGN_PASSWORD:
#   sign.ps1 dist\stage\demirror.exe dist\stage\vmon-server.exe ...
# Signed programs build up a reputation with Windows: SmartScreen and Defender stop
# warning about them, and Windows shows who published them. Without a certificate this
# does nothing (a self-signed one wouldn't help).

param([Parameter(Mandatory, ValueFromRemainingArguments)][string[]]$Files)
$ErrorActionPreference = "Stop"

if (-not $env:DEMIRROR_SIGN_PFX) {
    Write-Warning "no signing certificate (DEMIRROR_SIGN_PFX): $($Files -join ', ') left unsigned"
    exit 0
}

# signtool: on PATH in a developer prompt, else from the Windows SDK, else from NuGet.
$signtool = (Get-Command signtool -ErrorAction SilentlyContinue).Source
if (-not $signtool) {
    $signtool = Get-ChildItem "${env:ProgramFiles(x86)}\Windows Kits\10\bin\*\x64\signtool.exe" -ErrorAction SilentlyContinue |
        Sort-Object FullName | Select-Object -Last 1 -ExpandProperty FullName
}
if (-not $signtool) {
    $version = "10.0.26100.6584"
    $tools = Join-Path $env:TEMP "demirror-sdk-buildtools-$version"
    if (-not (Test-Path "$tools\bin")) {
        New-Item -ItemType Directory -Force $tools | Out-Null
        Invoke-WebRequest -UseBasicParsing -OutFile "$tools\tools.zip" `
            "https://api.nuget.org/v3-flatcontainer/microsoft.windows.sdk.buildtools/$version/microsoft.windows.sdk.buildtools.$version.nupkg"
        Expand-Archive -Force "$tools\tools.zip" $tools
    }
    $signtool = Get-ChildItem -Recurse "$tools\bin" -Filter signtool.exe | Where-Object FullName -like "*\x64\*" |
        Select-Object -First 1 -ExpandProperty FullName
}
if (-not $signtool) { throw "signtool.exe not found" }

$pfx = Join-Path $env:TEMP "demirror-sign-$PID.pfx"
[IO.File]::WriteAllBytes($pfx, [Convert]::FromBase64String($env:DEMIRROR_SIGN_PFX))
try {
    foreach ($file in $Files) {
        $common = @("sign", "/q", "/fd", "sha256", "/f", $pfx, "/p", $env:DEMIRROR_SIGN_PASSWORD, "/d", "demirror")
        # Timestamped, so the signature outlives the certificate.
        & $signtool @common /tr http://timestamp.digicert.com /td sha256 $file
        if ($LASTEXITCODE) {
            Write-Warning "timestamping $file failed, signing without it"
            & $signtool @common $file
            if ($LASTEXITCODE) { throw "signing $file failed" }
        }
        Write-Host "signed $file"
    }
} finally {
    Remove-Item -Force $pfx -ErrorAction SilentlyContinue
}
