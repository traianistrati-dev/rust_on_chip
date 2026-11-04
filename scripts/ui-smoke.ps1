<#
.SYNOPSIS
  Open a set of generated projects in the real IDE, walk its tabs, and capture
  each one - the visual half of verify-codegen.ps1.

.DESCRIPTION
  verify-codegen.ps1 proves the generated code builds. Nothing proved the IDE
  still DRAWS those projects: a panel that panics on one vendor, a tab that
  lost its content, a notice that never shows. Those were found by hand, one
  screenshot at a time. This is those checks, written down.

  Each case emits one project with its `#[ignore]`d harness (into a folder of
  its own under -Out), opens it in a DEBUG build started with
  EIDE_AUTOMATION, and then:
    - presses each MCU tab through UI Automation (AccessKit) and saves a
      capture of the IDE's window alone (PrintWindow - never the screen);
    - plays the case's own steps through the automation channel (scrolls,
      clicks on rows UI Automation cannot press, keys);
    - checks the texts the case expects are on screen, and the ones it
      expects NOT to be before a step;
    - fails when the IDE died, or panicked (its stderr, and the crash log a
      scripted run keeps in its own folder).
  The real mouse and keyboard are never touched, so it can run while you work.

  A scripted run takes a workspace slot and an app storage of its own
  (`workspace::AUTOMATION_SLOT`), keeps its crash log and project locks in its
  automation folder and stays out of the recent-projects list: your window,
  your remembered project, your history and your crash reports are left alone.
  That slot's storage is cleared before each case, so a case starts from the
  same window every time.

  Adding a case is one row in $CASES.

.PARAMETER Out
  Where the projects, the captures and report.md go. Default: a new folder
  under %TEMP%\eide-ui-smoke.

.PARAMETER Case
  Run only the cases whose name contains one of these (several may also be
  given as one comma-separated value, as `-File` passes them).

.PARAMETER NoBuild
  Use target\debug\rust_on_chip.exe as it is.

.NOTES
  Refuses to start while a codegen matrix run holds its lock, and holds that
  lock itself until it ends: the harnesses this runs share the repository's
  target\ with the matrix, and relinking the test binary under a running
  matrix breaks both.

.EXAMPLE
  powershell -File scripts/ui-smoke.ps1
  powershell -File scripts/ui-smoke.ps1 -Case f1,esp -Out I:\ui-smoke
#>
[CmdletBinding()]
param(
    [string]$Out = "",
    [string[]]$Case = @(),
    [switch]$NoBuild
)
$ErrorActionPreference = "Stop"
$repo = Split-Path $PSScriptRoot -Parent
$env:CARGO_HTTP_MULTIPLEXING = "false"

# Every MCU tab, in the order the IDE shows them. A tab a chip does not have
# is reported as absent, not as a failure - except these two.
$TABS = @("Pins", "Peripherals", "Configuration", "Clock", "System", "IoT")
$MUST = @("Pins", "System")
# The eframe storage of `workspace::AUTOMATION_SLOT` (`names::LEGACY_EFRAME_NAME`
# + `workspace::suffix()`): cleared before each case.
$SLOT_STORAGE = Join-Path $env:APPDATA "Embedded IDE_99"

# n: name (also the capture prefix)   t: emit test   e: its environment
# expect: tab -> texts that must be on screen there (substring of an
#         accessible name)
# steps:  after the tabs: @{ tab; absent (texts that must NOT be on screen
#         before the step); scroll = @{ at; dy } (the wheel over an element
#         first - negative dy shows what is below); click (an element's name)
#         | key; expect }
$CASES = @(
    @{ n = "f1-rtic-nested"; t = "emit_f1_rtic_project"; e = @{ EIDE_F1_TASKS = "nested" }
        expect = @{ System = @("blink (in tasks)", "tick (in tasks::hw)", "Dispatchers: RCC, FLASH") } }
    @{ n = "f1-rtic-custom"; t = "emit_f1_rtic_project"; e = @{ EIDE_CUSTOM = "1" }
        # Its row sits below the module list's fold in a fresh window; no
        # module is open before the click, so the panel after it is its.
        steps = @(@{ tab = "Pins"; absent = @("Remove module"); scroll = @{ at = "USART1"; dy = -400 }
                click = "Custom1 - panel"; expect = @("Remove module") }) }
    @{ n = "f1-native"; t = "emit_f1_native_project"; e = @{} }
    @{ n = "esp32c3-async"; t = "emit_esp32c3_project"; e = @{ EIDE_ESP_RUNTIME = "async" } }
    @{ n = "rp-pico"; t = "emit_rp_project"; e = @{} }
    @{ n = "microbit"; t = "emit_nrf_project"; e = @{} }
    @{ n = "embassy-stm32"; t = "emit_embassy_project"; e = @{} }
)
$names = @($Case | ForEach-Object { $_ -split "," } | ForEach-Object { $_.Trim() } | Where-Object { $_ })
if ($names.Count) {
    $CASES = @($CASES | Where-Object { $c = $_; $names | Where-Object { $c.n -like "*$_*" } })
    if (-not $CASES.Count) { throw "no case matches: $($names -join ', ')" }
}
# Every variable any case sets: cleared around each case, so one case's -
# or one the caller had set - never reaches another, and put back after.
$caseVars = @($CASES | ForEach-Object { $_.e.Keys } | Sort-Object -Unique)

# ── The matrix lock, held for the whole run ─────────────────────────────────
$lockPath = Join-Path $env:TEMP "eide-codegen-matrix.lock"
try {
    $script:lock = [System.IO.File]::Open($lockPath, 'OpenOrCreate', 'ReadWrite', 'None')
} catch {
    Write-Host "a codegen matrix run holds $lockPath - run this after it" -ForegroundColor Red
    exit 1
}
$savedVars = @{}
foreach ($k in $caseVars) { $savedVars[$k] = [Environment]::GetEnvironmentVariable($k, "Process") }
$origTmp = $env:TMP; $origTemp = $env:TEMP
$failed = 0
try {
    $env:EIDE_MATRIX_RUN = "1"
    if (-not $Out) { $Out = Join-Path $env:TEMP ("eide-ui-smoke\" + (Get-Date -Format "yyyyMMdd-HHmmss")) }
    New-Item -ItemType Directory -Force -Path $Out | Out-Null
    $Out = (Resolve-Path $Out).Path
    Write-Host "ui smoke -> $Out" -ForegroundColor DarkCyan

    $exe = Join-Path $repo "target\debug\rust_on_chip.exe"
    if (-not $NoBuild) {
        Write-Host "building the debug IDE..." -ForegroundColor DarkGray
        Push-Location $repo
        # Native tools: their stderr is progress, not an error (Windows
        # PowerShell turns it into one under "Stop").
        $ErrorActionPreference = "Continue"
        try { cargo build -q 2>&1 | Out-Host; if ($LASTEXITCODE) { throw "cargo build failed" } }
        finally { Pop-Location; $ErrorActionPreference = "Stop" }
    }
    if (-not (Test-Path $exe)) { throw "no $exe - build it, or drop -NoBuild" }

    # ── Window helpers: UI Automation, PrintWindow, the automation channel ──
    Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes, System.Drawing
    Add-Type @"
using System;
using System.Runtime.InteropServices;
public static class EideWin {
    [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr dc, uint f);
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
    [DllImport("user32.dll")] public static extern bool ScreenToClient(IntPtr h, ref POINT p);
    public struct RECT { public int L, T, R, B; }
    public struct POINT { public int X, Y; }
}
"@
    $UIA = [System.Windows.Automation.AutomationElement]

    function Get-Names($root) {
        $all = $root.FindAll([System.Windows.Automation.TreeScope]::Descendants,
            [System.Windows.Automation.Condition]::TrueCondition)
        foreach ($e in $all) { $n = $e.Current.Name; if ($n) { $n } }
    }
    function Find-Element($root, [string]$name) {
        $c = New-Object System.Windows.Automation.PropertyCondition($UIA::NameProperty, $name)
        $root.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $c)
    }
    function Press($el) {
        $pat = $null
        if ($el.TryGetCurrentPattern([System.Windows.Automation.TogglePattern]::Pattern, [ref]$pat)) { $pat.Toggle(); return $true }
        if ($el.TryGetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern, [ref]$pat)) { $pat.Invoke(); return $true }
        if ($el.TryGetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern, [ref]$pat)) { $pat.Select(); return $true }
        $false
    }
    # $false when there is no window left to capture.
    function Save-Window([IntPtr]$h, [string]$path) {
        $r = New-Object EideWin+RECT
        if (-not [EideWin]::GetWindowRect($h, [ref]$r)) { return $false }
        $w = $r.R - $r.L; $hh = $r.B - $r.T
        if ($w -le 0 -or $hh -le 0) { return $false }
        $bmp = New-Object System.Drawing.Bitmap $w, $hh
        $g = [System.Drawing.Graphics]::FromImage($bmp)
        $dc = $g.GetHdc()
        [void][EideWin]::PrintWindow($h, $dc, 2)
        $g.ReleaseHdc($dc)
        $g.Dispose()
        $bmp.Save($path, [System.Drawing.Imaging.ImageFormat]::Png)
        $bmp.Dispose()
        $true
    }
    $script:seq = [DateTime]::UtcNow.Ticks
    # One command file through the channel (src/app/automation.rs), whole:
    # written beside it and swapped in. Returns $null when played, else why not.
    function Send-Commands([string]$auto, [string[]]$lines) {
        $script:seq++
        $body = (@("seq $script:seq") + $lines + @("end")) -join "`n"
        $tmp = Join-Path $auto "cmd.tmp"
        $cmd = Join-Path $auto "cmd.txt"
        [System.IO.File]::WriteAllText($tmp, $body)
        if (Test-Path $cmd) { [System.IO.File]::Replace($tmp, $cmd, [NullString]::Value) }
        else { [System.IO.File]::Move($tmp, $cmd) }
        $done = Join-Path $auto "done.txt"
        for ($i = 0; $i -lt 100; $i++) {
            Start-Sleep -Milliseconds 100
            # The IDE may be writing it this very moment: try again.
            try { $text = [System.IO.File]::ReadAllText($done) } catch { continue }
            $r = $text -split "`n", 2
            if ($r[0].Trim() -eq "$script:seq") {
                Start-Sleep -Milliseconds 500
                if ($r.Count -gt 1 -and $r[1].Trim()) { return $r[1].Trim() }
                return $null
            }
        }
        "the IDE did not play it within 10 s"
    }
    # The centre of the element named $name, in the window's client pixels.
    function Get-ClientPoint([IntPtr]$h, $root, [string]$name) {
        $el = Find-Element $root $name
        if (-not $el) { return $null }
        $b = $el.Current.BoundingRectangle
        $pt = New-Object EideWin+POINT
        $pt.X = [int]($b.X + $b.Width / 2); $pt.Y = [int]($b.Y + $b.Height / 2)
        [void][EideWin]::ScreenToClient($h, [ref]$pt)
        $pt
    }
    function Test-Present($root, [string[]]$texts) {
        $all = @(Get-Names $root)
        foreach ($t in $texts) { if (-not ($all | Where-Object { $_ -like "*$t*" })) { "missing '$t'" } }
    }
    function Test-Absent($root, [string[]]$texts) {
        $all = @(Get-Names $root)
        foreach ($t in $texts) { if ($all | Where-Object { $_ -like "*$t*" }) { "'$t' already on screen" } }
    }
    # The IDE and everything it started (rust-analyzer, cargo, rustc), even
    # when the IDE itself is already gone. Not `taskkill /T`: it hung for hours
    # on a tree in the middle of a `cargo check`, and under "Stop" its stderr
    # about a child that had just exited ended the whole run. A process counts
    # as a descendant only if it started after the IDE did, so a reused PID
    # never takes a stranger with it. A few sweeps catch what a dying parent
    # spawned meanwhile.
    function Stop-Tree($p, [datetime]$since) {
        for ($sweep = 0; $sweep -lt 3; $sweep++) {
            $all = @(Get-CimInstance Win32_Process -Property ProcessId, ParentProcessId, CreationDate)
            $ids = New-Object System.Collections.Generic.List[int]
            $ids.Add($p.Id)
            for ($i = 0; $i -lt $ids.Count; $i++) {
                foreach ($c in $all) {
                    if ($c.ParentProcessId -eq $ids[$i] -and $c.CreationDate -ge $since -and
                        -not $ids.Contains([int]$c.ProcessId)) { $ids.Add([int]$c.ProcessId) }
                }
            }
            $live = @($ids | Where-Object { Get-Process -Id $_ -ErrorAction SilentlyContinue })
            if (-not $live.Count) { break }
            foreach ($id in $live) { try { Stop-Process -Id $id -Force -ErrorAction Stop } catch { } }
            Start-Sleep -Milliseconds 500
        }
        try { [void]$p.WaitForExit(10000) } catch { }
    }

    # ── Cases ────────────────────────────────────────────────────────────────
    $rows = New-Object System.Collections.Generic.List[object]
    foreach ($c in $CASES) {
        $fails = New-Object System.Collections.Generic.List[string]
        $dir = Join-Path $Out $c.n
        New-Item -ItemType Directory -Force -Path $dir | Out-Null
        $auto = Join-Path $dir "auto"
        $p = $null
        Write-Host ("{0,-16} emit" -f $c.n) -NoNewline
        try {
            $proj = $null
            Push-Location $repo
            try {
                foreach ($k in $caseVars) { Remove-Item "Env:$k" -ErrorAction SilentlyContinue }
                foreach ($k in $c.e.Keys) { Set-Item "Env:$k" $c.e[$k] }
                $env:TMP = $dir; $env:TEMP = $dir
                $ErrorActionPreference = "Continue"
                $emit = cargo test --bins $c.t -- --ignored --nocapture 2>&1 | ForEach-Object { "$_" }
                $emitCode = $LASTEXITCODE
            } finally {
                $ErrorActionPreference = "Stop"
                $env:TMP = $origTmp; $env:TEMP = $origTemp
                foreach ($k in $c.e.Keys) { Remove-Item "Env:$k" -ErrorAction SilentlyContinue }
                Pop-Location
            }
            # `wrote <path>`, or `wrote <path> (Display Name)`; the path may
            # hold spaces.
            foreach ($line in $emit) {
                if ($line -match "^wrote (.+?)(?: \([^()]*\))?\s*$" -and (Test-Path $matches[1])) {
                    $proj = $matches[1]; break
                }
            }
            if (-not $proj) {
                $emit | Set-Content -Path (Join-Path $dir "emit.log") -Encoding ascii
                throw "the harness wrote no project (exit $emitCode, emit.log)"
            }

            Write-Host " | open" -NoNewline
            Get-ChildItem $SLOT_STORAGE -Recurse -Filter "app.ron" -ErrorAction SilentlyContinue |
                Remove-Item -Force -ErrorAction SilentlyContinue
            $env:EIDE_AUTOMATION = $auto
            # Not attached to this console: its stderr stays in stderr.log.
            $env:RUST_ON_CHIP_SPAWNED_WINDOW = "1"
            # CIM's CreationDate has whole microseconds; a second of slack.
            $started = (Get-Date).AddSeconds(-1)
            try {
                $p = Start-Process -FilePath $exe -ArgumentList "`"$proj`"" -PassThru `
                    -RedirectStandardError (Join-Path $dir "stderr.log") `
                    -RedirectStandardOutput (Join-Path $dir "stdout.log")
            } finally {
                Remove-Item Env:EIDE_AUTOMATION, Env:RUST_ON_CHIP_SPAWNED_WINDOW -ErrorAction SilentlyContinue
            }
            # The first handle can be a short-lived one, the real window opens
            # small and is maximized a moment later, and the accessible tree
            # fills in after the first frames: wait for the Pins tab, or
            # "Restoring project" below is looked for too early.
            $h = [IntPtr]::Zero; $root = $null
            for ($i = 0; $i -lt 240 -and -not $p.HasExited; $i++) {
                Start-Sleep -Milliseconds 500
                $p.Refresh()
                if ($p.MainWindowHandle -eq [IntPtr]::Zero) { continue }
                $h = $p.MainWindowHandle
                try {
                    $r = New-Object EideWin+RECT
                    [void][EideWin]::GetWindowRect($h, [ref]$r)
                    $root = $UIA::FromHandle($h)
                    if (($r.R - $r.L) -gt 800 -and (Find-Element $root "Pins")) { break }
                } catch { }
                $root = $null
            }
            if ($p.HasExited) { throw "the IDE exited while opening (code $($p.ExitCode))" }
            if (-not $root) { throw "no window with a Pins tab within 2 min" }
            # The project loads behind an overlay; the tabs are drawn under it.
            for ($i = 0; $i -lt 90 -and -not $p.HasExited; $i++) {
                if (-not (Get-Names $root | Where-Object { $_ -like "Restoring project*" })) { break }
                Start-Sleep -Seconds 2
            }
            Start-Sleep -Seconds 2
            $png = "$($c.n)-start.png"
            if (Save-Window $h (Join-Path $Out $png)) {
                $rows.Add([pscustomobject]@{ case = $c.n; what = "opened"; result = "ok"; png = $png })
            }

            Write-Host " | tabs" -NoNewline
            foreach ($tab in $TABS) {
                if ($p.HasExited) { break }
                $el = Find-Element $root $tab
                if (-not $el) {
                    if ($MUST -contains $tab) { $fails.Add("no $tab tab") }
                    $rows.Add([pscustomobject]@{ case = $c.n; what = $tab; result = "absent"; png = "" })
                    continue
                }
                if (-not (Press $el)) { $fails.Add("$tab cannot be pressed"); continue }
                Start-Sleep -Milliseconds 1500
                $png = "$($c.n)-$tab.png"
                if (-not (Save-Window $h (Join-Path $Out $png))) { break }
                $miss = @(if ($c.expect -and $c.expect[$tab]) { Test-Present $root $c.expect[$tab] })
                foreach ($m in $miss) { $fails.Add("$tab - $m") }
                $rows.Add([pscustomobject]@{ case = $c.n; what = $tab
                    result = $(if ($miss) { $miss -join "; " } else { "ok" }); png = $png })
            }

            $n = 0
            foreach ($s in @($c.steps)) {
                if (-not $s -or $p.HasExited) { continue }
                $n++
                $tabEl = Find-Element $root $s.tab
                if ($tabEl) { [void](Press $tabEl); Start-Sleep -Milliseconds 1000 }
                $res = New-Object System.Collections.Generic.List[string]
                if ($s.absent) { foreach ($m in @(Test-Absent $root $s.absent)) { $res.Add("before: $m") } }
                if ($s.scroll -and -not $res.Count) {
                    $pt = Get-ClientPoint $h $root $s.scroll.at
                    if (-not $pt) { $res.Add("no element named '$($s.scroll.at)' to scroll over") }
                    else {
                        $why = Send-Commands $auto @("scroll $($pt.X) $($pt.Y) 0 $($s.scroll.dy)")
                        if ($why) { $res.Add($why) }
                    }
                }
                if (-not $res.Count) {
                    if ($s.click) {
                        $pt = Get-ClientPoint $h $root $s.click
                        if (-not $pt) { $res.Add("no element named '$($s.click)'") }
                        else { $why = Send-Commands $auto @("click $($pt.X) $($pt.Y)"); if ($why) { $res.Add($why) } }
                    } elseif ($s.key) {
                        $why = Send-Commands $auto @("key $($s.key)"); if ($why) { $res.Add($why) }
                    }
                }
                if (-not $res.Count -and $s.expect) { foreach ($m in @(Test-Present $root $s.expect)) { $res.Add($m) } }
                foreach ($m in $res) { $fails.Add("step $n - $m") }
                $png = "$($c.n)-step$n.png"
                if (-not (Save-Window $h (Join-Path $Out $png))) { $png = "" }
                $what = "step $n " + $(if ($s.click) { "click '$($s.click)'" } else { "key $($s.key)" })
                $rows.Add([pscustomobject]@{ case = $c.n; what = $what
                    result = $(if ($res.Count) { $res -join "; " } else { "ok" }); png = $png })
            }
            if ($p.HasExited) { $fails.Add("the IDE exited (code $($p.ExitCode))") }
        } catch {
            $fails.Add("$_")
        } finally {
            # Never throws: the next cases and the report still run.
            if ($p) { try { Stop-Tree $p $started } catch { $fails.Add("stopping the IDE: $_") } }
        }
        # A panic, wherever it was told: stderr, or the scripted run's crash log.
        $panic = @((Join-Path $dir "stderr.log"), (Join-Path $auto "crash.log")) |
            Where-Object { Test-Path $_ } |
            ForEach-Object { Select-String -Path $_ -Pattern "panicked" -SimpleMatch } |
            Select-Object -First 1
        if ($panic) { $fails.Add("panic: $($panic.Line.Trim())") }

        if ($fails.Count) {
            $failed++
            Write-Host " | FAIL" -ForegroundColor Red
            foreach ($f in $fails) { Write-Host "    $f" -ForegroundColor Red }
            $rows.Add([pscustomobject]@{ case = $c.n; what = "case"; result = ($fails -join "; "); png = "" })
        } else {
            Write-Host " | ok" -ForegroundColor Green
        }
    }

    # ── Report ───────────────────────────────────────────────────────────────
    $md = New-Object System.Collections.Generic.List[string]
    $md.Add("# UI smoke $(Get-Date -Format 'yyyy-MM-dd HH:mm')")
    $md.Add("")
    $md.Add("$($CASES.Count - $failed) of $($CASES.Count) cases passed.")
    $md.Add("")
    $md.Add("| case | what | result | capture |")
    $md.Add("|---|---|---|---|")
    foreach ($r in $rows) {
        $link = if ($r.png) { "[$($r.png)]($($r.png))" } else { "" }
        $md.Add("| $($r.case) | $($r.what) | $($r.result -replace '\|', '/') | $link |")
    }
    [System.IO.File]::WriteAllLines((Join-Path $Out "report.md"), $md)
    Write-Host ""
    Write-Host ("{0} of {1} cases passed - {2}" -f ($CASES.Count - $failed), $CASES.Count, (Join-Path $Out "report.md")) `
        -ForegroundColor $(if ($failed) { "Red" } else { "Green" })
} finally {
    $env:TMP = $origTmp; $env:TEMP = $origTemp
    Remove-Item Env:EIDE_MATRIX_RUN -ErrorAction SilentlyContinue
    foreach ($k in $savedVars.Keys) { [Environment]::SetEnvironmentVariable($k, $savedVars[$k], "Process") }
    $script:lock.Dispose()
}
exit $(if ($failed) { 1 } else { 0 })
