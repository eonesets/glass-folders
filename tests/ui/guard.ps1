# Кратко: проверка «сторожа». Запускает программу на временном столе (окна невидимые), убивает её разными способами
# и проверяет, что папки снова видны (атрибут «скрытый» снят) и settings.homes очищен.
#
# Usage: powershell -File tests/ui/guard.ps1
# Kills only processes this script started (by PID), never by image name. Real Desktop untouched (GF_DESKTOP).

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $MyInvocation.MyCommand.Path
$exe = Join-Path $root '..\..\app\target\release\glass-folders.exe' | Resolve-Path
$tmp = Join-Path $env:TEMP 'gf-test\guard'
$fail = 0

function Fresh {
    if (Test-Path $tmp) { Remove-Item $tmp -Recurse -Force }
    foreach ($d in 'desk\Work', 'desk\Games', 'appdata\glass-folders') { New-Item -ItemType Directory -Force (Join-Path $tmp $d) | Out-Null }
    'rem test' | Set-Content (Join-Path $tmp 'desk\Work\a.bat')
    'rem test' | Set-Content (Join-Path $tmp 'desk\Games\b.bat')
    '{"lang":0}' | Set-Content (Join-Path $tmp 'appdata\glass-folders\settings.json')
}

function Hidden($name) { [bool]((Get-Item -Force (Join-Path $tmp "desk\$name")).Attributes -band [IO.FileAttributes]::Hidden) }
function Homes { @((Get-Content (Join-Path $tmp 'appdata\glass-folders\settings.json') -Raw | ConvertFrom-Json).homes.PSObject.Properties).Count }

function Start-App {
    $psi = New-Object System.Diagnostics.ProcessStartInfo $exe
    $psi.UseShellExecute = $false
    $psi.EnvironmentVariables['GF_DESKTOP'] = Join-Path $tmp 'desk'
    $psi.EnvironmentVariables['APPDATA'] = Join-Path $tmp 'appdata'
    $psi.EnvironmentVariables['GF_OFFSCREEN'] = '1'
    $p = [System.Diagnostics.Process]::Start($psi)
    for ($i = 0; $i -lt 50 -and -not (Hidden 'Work'); $i++) { Start-Sleep -Milliseconds 100 }
    Start-Sleep -Milliseconds 300 # guard up
    return $p
}

function Check($what, [scriptblock]$kill) {
    Fresh
    $p = Start-App
    $hid = (Hidden 'Work') -and (Hidden 'Games')
    $guards = @(Get-CimInstance Win32_Process -Filter "Name='glass-folders.exe'" | Where-Object { $_.CommandLine -like "*--guard $($p.Id)*" })
    & $kill $p
    $ok = $false
    for ($i = 0; $i -lt 50; $i++) {
        Start-Sleep -Milliseconds 100
        if (-not (Hidden 'Work') -and -not (Hidden 'Games') -and (Homes) -eq 0) { $ok = $true; break }
    }
    $ok = $ok -and $hid
    "{0}  {1}  (hidden while running: {2}, guard: {3}, restored in ~{4} ms)" -f ($(if ($ok) { 'PASS' } else { 'FAIL' })), $what, $hid, $guards.Count, ($i * 100)
    foreach ($g in $guards) { Stop-Process -Id $g.ProcessId -Force -ErrorAction SilentlyContinue }
    if (-not $ok) { $script:fail++ }
}

Check 'hard kill (Task Manager End task / TerminateProcess)' { param($p) Stop-Process -Id $p.Id -Force }
Check 'kill process tree (taskkill /T /F)' { param($p) taskkill /PID $p.Id /T /F | Out-Null }
Check 'close request (taskkill without /F)' { param($p) taskkill /PID $p.Id | Out-Null }
exit $fail
