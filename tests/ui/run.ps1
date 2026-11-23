# Кратко: запускает UI-сценарии без настоящей мыши. Делает временный «рабочий стол» и свои настройки,
# прогоняет каждый scenarios/*.gfs через glass-folders.exe (GF_SCRIPT), печатает PASS/FAIL. Картинки в tests/ui/out/<сценарий>/.
#
# Usage: powershell -File tests/ui/run.ps1 [name ...]   (no names = all scenarios)
# Needs: app/target/release/glass-folders.exe (cargo build --release in app/).
# Safe while the user works: windows invisible + click-through, no focus steal, real cursor never touched,
# demo desktop in %TEMP% (real Desktop and real settings untouched).

param([Parameter(ValueFromRemainingArguments = $true)][string[]]$Names)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $MyInvocation.MyCommand.Path
$exe = Join-Path $root '..\..\app\target\release\glass-folders.exe' | Resolve-Path
$outAll = Join-Path $root 'out'
$scen = Get-ChildItem (Join-Path $root 'scenarios') -Filter *.gfs | Where-Object { -not $Names -or $Names -contains $_.BaseName }

# Fresh demo desktop: folder Work = groups Docs, Tools + loose files (Misc group).
function New-Fixture($dir) {
    if (Test-Path $dir) { Remove-Item $dir -Recurse -Force }
    $w = Join-Path $dir 'Work'
    foreach ($d in 'Docs', 'Tools') { New-Item -ItemType Directory -Force (Join-Path $w $d) | Out-Null }
    # items = shortcut types only (model::is_item); .bat that do nothing, in case one is ever launched
    foreach ($f in 'Docs\alpha.bat', 'Docs\beta.bat', 'Tools\gamma.bat', 'notes.bat') { 'rem test' | Set-Content (Join-Path $w $f) }
}

$fail = 0
foreach ($s in $scen) {
    $tmp = Join-Path $env:TEMP "gf-test\$($s.BaseName)"
    New-Fixture (Join-Path $tmp 'desk')
    $cfg = Join-Path $tmp 'appdata\glass-folders'
    if (Test-Path $cfg) { Remove-Item $cfg -Recurse -Force }
    New-Item -ItemType Directory -Force $cfg | Out-Null
    # fixed settings: Russian UI, rest default (same pictures on any PC). Scenario line '#settings {json}' overrides.
    $set = (Get-Content $s.FullName -Encoding UTF8 | Where-Object { $_ -like '#settings *' } | Select-Object -First 1)
    $set = if ($set) { $set.Substring(10) } else { '{"lang":0}' }
    $set | Set-Content (Join-Path $cfg 'settings.json')
    $out = Join-Path $outAll $s.BaseName
    if (Test-Path $out) { Remove-Item $out -Recurse -Force }

    $psi = New-Object System.Diagnostics.ProcessStartInfo $exe
    $psi.UseShellExecute = $false
    $psi.EnvironmentVariables['GF_SCRIPT'] = $s.FullName
    $psi.EnvironmentVariables['GF_OUT'] = $out
    $psi.EnvironmentVariables['GF_DESKTOP'] = Join-Path $tmp 'desk'
    $psi.EnvironmentVariables['APPDATA'] = Join-Path $tmp 'appdata'
    $psi.EnvironmentVariables['GF_STAY'] = '1'
    $p = [System.Diagnostics.Process]::Start($psi)
    if (-not $p.WaitForExit(90000)) { $p.Kill(); $code = 2 } else { $code = $p.ExitCode }
    $res = if ($code -eq 0) { 'PASS' } else { 'FAIL'; $fail++ }
    Write-Output ("{0,-5} {1}  (exit {2}, log {3})" -f $res, $s.BaseName, $code, (Join-Path $out 'log.txt'))
    if ($code -ne 0 -and (Test-Path (Join-Path $out 'log.txt'))) { Get-Content (Join-Path $out 'log.txt') -Tail 5 }
}
exit $fail
