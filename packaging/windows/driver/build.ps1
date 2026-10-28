# Builds and signs demirror's display driver into $Out: demirror_idd.dll, .inf, .cat and the
# certificate it's signed with (demirror.cer).
#
# Needs Visual Studio (or its Build Tools) with the C++ tools; a developer prompt is set up if
# this isn't run from one. The Windows Driver Kit's headers and libraries come from NuGet, so
# no WDK install is needed.
#
# Signing: with $env:DEMIRROR_SIGN_PFX (a base64 .pfx) and $env:DEMIRROR_SIGN_PASSWORD the
# driver is signed with that certificate. Otherwise a new self-signed certificate is made for
# this build and its private key is thrown away; installing the driver then trusts its
# certificate on that computer (see install-driver in the app).

param(
    [string]$Out = "dist\driver",
    [string]$Version = "0.1.0"
)
$ErrorActionPreference = "Stop"
$here = $PSScriptRoot
$wdkVersion = "10.0.26100.6584"

# Outside a developer prompt, set one up from the installed Visual Studio (or Build Tools).
if (-not (Get-Command cl -ErrorAction SilentlyContinue)) {
    $vswhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
    $vs = & $vswhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
    if (-not $vs) { throw "Visual Studio with the C++ tools is needed" }
    cmd /c "`"$vs\VC\Auxiliary\Build\vcvars64.bat`" >nul && set" | ForEach-Object {
        if ($_ -match "^([^=]+)=(.*)$") { Set-Item "env:$($Matches[1])" $Matches[2] }
    }
}
foreach ($tool in "cl", "link", "signtool") {
    if (-not (Get-Command $tool -ErrorAction SilentlyContinue)) {
        throw "$tool not found: run from a Visual Studio x64 developer prompt"
    }
}

# The WDK, from NuGet.
$cache = Join-Path $env:TEMP "demirror-wdk-$wdkVersion"
if (-not (Test-Path "$cache\c")) {
    New-Item -ItemType Directory -Force $cache | Out-Null
    $package = "$cache\wdk.zip"
    Invoke-WebRequest -UseBasicParsing -OutFile $package `
        "https://api.nuget.org/v3-flatcontainer/microsoft.windows.wdk.x64/$wdkVersion/microsoft.windows.wdk.x64.$wdkVersion.nupkg"
    Expand-Archive -Force $package $cache
}
$c = "$cache\c"
$sdk = (Get-ChildItem "$c\Include" -Directory | Where-Object Name -like "10.*" | Sort-Object Name | Select-Object -Last 1).Name
Write-Host "WDK $wdkVersion, headers $sdk"

$build = Join-Path $env:TEMP "demirror-idd-build"
Remove-Item -Recurse -Force $build -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force $build, $Out | Out-Null
$Out = (Resolve-Path $Out).Path

# The installed SDK's headers and libraries come first (from the developer prompt); the WDK
# adds UMDF, IddCx and the driver headers the SDK lacks.
$includes = @("$c\Include\wdf\umdf\2.15", "$c\Include\$sdk\um\iddcx\1.2", $here,
              "$c\Include\$sdk\um", "$c\Include\$sdk\shared")
$libs = @("$c\Lib\wdf\umdf\x64\2.15", "$c\Lib\$sdk\um\x64\iddcx\1.2", "$c\Lib\$sdk\um\x64")

$defines = "UMDF_VERSION_MAJOR=2", "UMDF_VERSION_MINOR=15", "UMDF_USING_NTSTATUS", "UNICODE", "_UNICODE"
# C4324: padding in the WDK's own headers (WDK projects turn it off too).
$clArgs = @("/nologo", "/c", "/std:c++17", "/EHsc", "/O2", "/MT", "/W4", "/WX", "/wd4324", "/GS", "/guard:cf",
            "/Fo$build\", "$here\demirror_idd.cpp")
$clArgs += $defines | ForEach-Object { "/D$_" }
$clArgs += $includes | ForEach-Object { "/I$_" }
& cl @clArgs
if ($LASTEXITCODE) { throw "compiling the driver failed" }

$linkArgs = @("/nologo", "/DLL", "/OUT:$Out\demirror_idd.dll", "/OPT:REF", "/OPT:ICF", "/guard:cf",
              "/DYNAMICBASE", "/NXCOMPAT", "$build\demirror_idd.obj",
              "WdfDriverStubUm.lib", "IddCxStub.lib", "ntdll.lib", "d3d11.lib", "dxgi.lib", "kernel32.lib")
$linkArgs += $libs | ForEach-Object { "/LIBPATH:$_" }
& link @linkArgs
if ($LASTEXITCODE) { throw "linking the driver failed" }

# The INF, dated and versioned for this build.
$parts = @($Version -replace "-.*", "" -split "\." | ForEach-Object { [int]$_ })
while ($parts.Count -lt 4) { $parts += 0 }
$driverVer = "{0},{1}" -f (Get-Date -Format "MM/dd/yyyy"), ($parts -join ".")
(Get-Content "$here\demirror_idd.inf" -Raw) -replace "@DRIVERVER@", $driverVer |
    Set-Content -NoNewline -Encoding ascii "$Out\demirror_idd.inf"

# The signing certificate.
$store = "Cert:\CurrentUser\My"
if ($env:DEMIRROR_SIGN_PFX) {
    $pfx = "$build\sign.pfx"
    [IO.File]::WriteAllBytes($pfx, [Convert]::FromBase64String($env:DEMIRROR_SIGN_PFX))
    $password = ConvertTo-SecureString -AsPlainText -Force $env:DEMIRROR_SIGN_PASSWORD
    $cert = Import-PfxCertificate -FilePath $pfx -CertStoreLocation $store -Password $password
    Remove-Item $pfx
} else {
    $cert = New-SelfSignedCertificate -Type CodeSigningCert -Subject "CN=demirror display driver" `
        -CertStoreLocation $store -KeyExportPolicy NonExportable -NotAfter (Get-Date).AddYears(10)
}
if (-not $env:DEMIRROR_SIGN_PFX) {
    # Installing trusts this certificate; a certificate from a real authority needs no help.
    Export-Certificate -Cert $cert -FilePath "$Out\demirror.cer" | Out-Null
}

function Sign($file) {
    & signtool sign /q /fd sha256 /sha1 $cert.Thumbprint /s My /tr http://timestamp.digicert.com /td sha256 $file
    if ($LASTEXITCODE) {
        Write-Warning "timestamping failed, signing without it"
        & signtool sign /q /fd sha256 /sha1 $cert.Thumbprint /s My $file
        if ($LASTEXITCODE) { throw "signing $file failed" }
    }
}

try {
    Sign "$Out\demirror_idd.dll"
    $inf2cat = Get-ChildItem -Recurse "$c\bin" -Filter Inf2Cat.exe | Select-Object -First 1
    & $inf2cat.FullName /driver:$Out /os:10_X64 /uselocaltime
    if ($LASTEXITCODE) { throw "making the driver catalog failed" }
    Sign "$Out\demirror_idd.cat"
} finally {
    # The private key isn't kept: nothing else can be signed with this certificate.
    Remove-Item -Force "$store\$($cert.Thumbprint)" -DeleteKey -ErrorAction SilentlyContinue
}
Write-Host "driver ready in $Out"
