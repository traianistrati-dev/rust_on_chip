<#
.SYNOPSIS
  Emit generated projects for a matrix of configurations and cross-compile them.

.DESCRIPTION
  The `#[ignore]`d emit tests each write ONE project to a temp directory. That
  is the only way this repo can find out whether the code it generates actually
  builds, and every codegen bug found so far was found by running one of them by
  hand — which means the ones nobody thought to run stayed broken for months:
  half-wired buses that named undeclared bindings, a CAN path that had never
  compiled at all, a PWM example printed against an API that no longer existed.

  This script is those runs, written down. It drives each emit test with the
  environment the case needs, reads the `wrote <path>` / `target: <triple>`
  lines the test prints, cross-compiles what it finds, and reports one line per
  case. Adding a case is one row in $ALL_CASES.

.PARAMETER Full
  Run every case. Without it, a representative subset that still covers each
  runtime and each "half-wired" shape.

.NOTES
  Warnings are checked too, and by COUNT: each case declares how many it is
  allowed (`w`, default none) and anything else fails. There is no "warnings are
  fine" mode — that is precisely how five of them lived in the ESP backend, on
  the fully-wired path, until this matrix first covered it.

.EXAMPLE
  pwsh scripts/verify-codegen.ps1
  pwsh scripts/verify-codegen.ps1 -Full
#>
[CmdletBinding()]
param(
    [switch]$Full,
    # EVERY case of the named families (f1, esp, rp, nrf, embassy, import, n6) -
    # what the pre-push hook runs for a push confined to them. It used to be one
    # representative case per family, because a family cost minutes per case:
    # each harness deleted its project's target/ and rebuilt every dependency.
    # With the cache kept, a whole family costs 0.5-3.5 minutes warm (F1's 21
    # cases 146 s, ESP's 14 179 s, measured 2026-10-02), so the hook no longer
    # has to guess which case stands for the rest.
    #
    # Names it does not recognise are an ERROR, not an empty run. A hook that
    # silently verified nothing would be worse than no hook at all.
    [string[]]$Hook = @(),
    # Where the emitted projects and their `target/` directories go.
    #
    # A full run leaves about 16 GB behind, and nothing cleans it up: eight runs
    # filled a 465 GB C: to exactly zero bytes, at which point cargo fails with
    # `could not compile syn` and the matrix looks like a codegen regression.
    # Pointing this at a roomier volume is the fix.
    #
    # Three places, in order: this switch, $env:EIDE_MATRIX_DIR, and a
    # `scripts/matrix-dir.txt` holding one path.
    #
    # The file exists so the choice can be made ONCE per clone without setting a
    # persistent Windows environment variable - that writes to HKCU\Environment,
    # follows the user into every program they run, and is a heavier thing to
    # leave behind than a gitignored text file in the repository it serves.
    [string]$WorkDir = $env:EIDE_MATRIX_DIR,
    # Wipe the work directory before starting. A deliberate COLD run: every
    # dependency recompiles, which is minutes, so it is a switch and not the
    # default. Stale directories are pruned automatically after a full green
    # run without it - see the prune below.
    [switch]$Clean
)

$ErrorActionPreference = "Continue"
$repo = Split-Path -Parent $PSScriptRoot

# `rtic-macros` counts every CARGO_FEATURE_* variable it can see as an enabled
# feature, cargo's own and the shell's alike, and refuses to build when there is
# more than one. A stray one in the user environment therefore breaks every RTIC
# case with an error that points at a correct Cargo.toml. The IDE reports this
# at startup (see `required_tools.rs`); here we simply do not pass it on.
$leaked = @(Get-ChildItem Env: | Where-Object { $_.Name -like "CARGO_FEATURE_*" })
foreach ($v in $leaked) { Remove-Item ("Env:\" + $v.Name) -ErrorAction SilentlyContinue }
if ($leaked) {
    Write-Host ("note: ignoring {0} stray CARGO_FEATURE_* variable(s) from this environment: {1}" -f
        $leaked.Count, ($leaked.Name -join ", ")) -ForegroundColor DarkYellow
}

# ONE run at a time, machine-wide.
#
# Every case writes to a FIXED directory under %TEMP%, so two runs share one
# `target/` and tear each other's artifacts apart. The damage does not look like
# concurrency: it surfaces as `could not write output`, `failed to write dep
# info`, `failed to write fingerprint`, `link.exe: 1104` — six "ERRORS" that
# read as a codegen regression and point at the wrong file entirely.
#
# Not hypothetical, and not rare either: it happened twice in one evening, the
# second time because a `git push` fired the pre-push hook while a run was
# already going. Anyone with the hook installed can trigger it without knowing
# a run exists.
#
# WAIT rather than refuse: this runs inside a pre-push hook, and a hook that
# exits non-zero ABORTS THE PUSH. Making someone's push fail because a matrix
# was running is a worse answer than making it wait.
#
# The lock is an OS file handle, so it is released even if this script is killed
# — the same reason `src/workspace.rs` locks that way rather than with a
# pid file.
$lockPath = Join-Path $env:TEMP "eide-codegen-matrix.lock"
# The lock is held with NO sharing, so the holder cannot describe itself through
# it — a waiter cannot even read it. Hence a sidecar, written just after the
# lock is taken: it is the only way "who am I waiting for" can be answered.
$ownerPath = Join-Path $env:TEMP "eide-codegen-matrix.owner"
$script:lock = $null
$waited = 0
while (-not $script:lock) {
    try {
        $script:lock = [System.IO.File]::Open($lockPath, 'OpenOrCreate', 'ReadWrite', 'None')
    } catch {
        if ($waited -eq 0) {
            # Say WHO, and say how to leave. A pre-push hook that goes quiet for
            # a quarter of an hour is indistinguishable from a hung push, and
            # the person waiting has no way to find out which it is.
            #
            # Not a prompt: a hook's stdin is git's ref list, so `Read-Host`
            # reads EOF, and reading the console instead would hang every
            # BACKGROUND run of this script forever. A long wait is a nuisance;
            # an unbounded one is a regression.
            $who = ""
            if (Test-Path $ownerPath) {
                $who = (Get-Content $ownerPath -Raw -ErrorAction SilentlyContinue).Trim()
            }
            if ($who) {
                Write-Host "another codegen matrix run holds the lock ($who)" -ForegroundColor DarkYellow
            } else {
                Write-Host "another codegen matrix run holds $lockPath" -ForegroundColor DarkYellow
            }
            Write-Host "waiting for it. To push without waiting: Ctrl+C, then 'git push --no-verify'" -ForegroundColor DarkYellow
        } elseif ($waited % 60 -eq 0) {
            Write-Host ("  still waiting - {0} min so far" -f [math]::Floor($waited / 60)) -ForegroundColor DarkGray
        }
        if ($waited -ge 2400) {
            Write-Host "gave up after 40 min waiting for $lockPath" -ForegroundColor Red
            exit 1
        }
        Start-Sleep -Seconds 5
        $waited += 5
    }
}
if ($waited -gt 0) { Write-Host ("waited {0} min for the lock" -f [math]::Round($waited / 60, 1)) -ForegroundColor DarkYellow }
# Whoever waits next reads this. Stale entries are harmless: it is only ever
# read by someone who has just seen the lock held.
# ASCII, not utf8: Windows PowerShell writes a BOM for `-Encoding utf8`, and it
# shows up inside the "(PID ...)" the next waiter prints. The content is ASCII.
"PID $PID, started $(Get-Date -Format 'HH:mm:ss')" | Set-Content -Path $ownerPath -Encoding ascii

# The harnesses warn when they find this lock held, because running one by hand
# during a matrix run corrupts both. Our own children are exactly the case that
# is fine, so tell them so.
$env:EIDE_MATRIX_RUN = "1"

# Relocate the projects, but NOT the lock.
#
# The lock was taken above from the ORIGINAL %TEMP% on purpose: two runs writing
# to different volumes still have to serialise, and a lock that moved with the
# work would let them run on top of each other in the one repository they share.
#
# `std::env::temp_dir()` in the harnesses reads TMP first, then TEMP, at the
# moment it is called - and cargo inherits this environment - so setting both
# here moves the emitted projects and the `target/` inside each of them.
# Restored on the way out, whatever happens. `& .erify-codegen.ps1` runs in the
# CALLER's process, so a TMP/TEMP left pointing at the work volume follows the
# user into every later command in that shell - including ones that have nothing
# to do with this repository.
# The per-clone default, when neither the switch nor the variable named one.
if (-not $WorkDir) {
    $dirFile = Join-Path $PSScriptRoot "matrix-dir.txt"
    if (Test-Path $dirFile) {
        $WorkDir = (Get-Content $dirFile -Raw -ErrorAction SilentlyContinue).Trim()
    }
}

$script:touched = @{}
$script:workRoot = $null
$script:origTmp = $env:TMP
$script:origTemp = $env:TEMP
trap { $env:TMP = $script:origTmp; $env:TEMP = $script:origTemp; break }

if ($WorkDir) {
    if (-not (Test-Path $WorkDir)) {
        New-Item -ItemType Directory -Force -Path $WorkDir | Out-Null
    }
    # NOT `$full`: PowerShell variable names are case-insensitive, so that name
    # is the `[switch]$Full` parameter above, and assigning a path to it fails
    # with a SwitchParameter conversion error - after which TEMP became the
    # string "False" and every case failed in the linker. The same collision
    # that made `$cases`/`$CASES` print the wrong count.
    $workRoot = (Resolve-Path $WorkDir).Path
    $script:workRoot = $workRoot
    if ($Clean) {
        Write-Host "  -Clean: emptying it first, so this run is cold" -ForegroundColor Yellow
        Get-ChildItem $workRoot -Directory -Filter "eide*" -ErrorAction SilentlyContinue |
            Remove-Item -Recurse -Force -ErrorAction SilentlyContinue
    }
    $env:TMP = $workRoot
    $env:TEMP = $workRoot
    $vol = $workRoot.Substring(0, 2)
    $free = (Get-CimInstance Win32_LogicalDisk -Filter "DeviceID='$vol'").FreeSpace
    Write-Host ("projects -> {0}  ({1:N1} GB free)" -f $workRoot, ($free / 1GB)) -ForegroundColor DarkCyan
    if ($free -lt 25GB) {
        Write-Host "  under 25 GB free - a full run needs about 16 GB" -ForegroundColor Yellow
    }
}

# Where the STM32Cube database is, if it is anywhere. The two importer cases
# need it; everything else is built from definitions bundled in the repo.
$CUBE_DB = if ($env:EIDE_CUBE_DB) { $env:EIDE_CUBE_DB }
           else { "H:\stm32cube-database-master\stm32cube-database-master\db\mcu" }

# label, emit test, environment for the run, quick?, prerequisite path, and `w`
# — how many warnings the case is allowed, default none.
#
# EXACTLY that many, not "at most": generated code is meant to be warning-free,
# and the few that remain are deliberate (a half-wired bus leaves its pad bound
# and unused, which is the compiler naming the same pad the generated comment
# names). Writing the number down is what makes a NEW warning fail the run —
# a threshold of "warnings are fine" is how five of them lived in the ESP
# backend, on the fully-wired path, until this matrix first covered it.
# It also fails when a case stops warning, so a fixed one cannot quietly keep
# its allowance.
#
# The env hash is the case: every key is a knob the emit test reads, and an
# empty hash means "as wired by default".
#
# `lk` LINKS as well: `cargo build --release`, the profile the IDE's Build
# uses, instead of `cargo check`. `cargo check` never runs the linker, so a
# layout bug is invisible to it: every RP2350 project put its IMAGE_DEF past
# the boot ROM's 4 KiB window and this matrix stayed green, because nothing it
# ran ever placed a section. memory.x now asserts the placement, and only a
# link evaluates the assert. It costs a codegen pass per project, so only the
# rows whose risk IS the layout carry it.
$ALL_CASES = @(
    @{ n = "F1 blocking, full wiring";     t = "emit_f1_dma_project";    e = @{ EIDE_F1_DMA = "off" };  q = $true; fam = "f1" }
    @{ n = "F1 blocking, DMA tx";          t = "emit_f1_dma_project";    e = @{ EIDE_F1_DMA = "tx" };   q = $false; fam = "f1" }
    @{ n = "F1 blocking, DMA rx";          t = "emit_f1_dma_project";    e = @{ EIDE_F1_DMA = "rx" };   q = $false; fam = "f1" }
    @{ n = "F1 blocking, DMA both";        t = "emit_f1_dma_project";    e = @{ EIDE_F1_DMA = "both" }; q = $true; fam = "f1" }
    @{ n = "F1 SPI without MISO";          t = "emit_f1_dma_project";    e = @{ EIDE_F1_DMA = "both"; EIDE_SPI_TXONLY = "1" }; q = $true; fam = "f1" }
    # `w` is how many warnings this case is ALLOWED — see the note above $ALL_CASES.
    # A half-wired bus leaves its pad bound and unused on purpose, and that
    # warning is the compiler naming the same pad the generated comment does.
    @{ n = "F1 USART TX only";             t = "emit_f1_dma_project";    e = @{ EIDE_F1_DMA = "off"; EIDE_USART_HALF = "tx" }; q = $true;  w = 2; fam = "f1" }
    @{ n = "F1 USART RX only";             t = "emit_f1_dma_project";    e = @{ EIDE_F1_DMA = "off"; EIDE_USART_HALF = "rx" }; q = $false; w = 2; fam = "f1" }
    @{ n = "F1 I2C SCL only";              t = "emit_f1_dma_project";    e = @{ EIDE_F1_DMA = "off"; EIDE_I2C_HALF = "scl" };  q = $true;  w = 2; fam = "f1" }
    @{ n = "F1 I2C SDA only";              t = "emit_f1_dma_project";    e = @{ EIDE_F1_DMA = "off"; EIDE_I2C_HALF = "sda" };  q = $false; w = 2; fam = "f1" }
    @{ n = "F1 CAN TX only";               t = "emit_f1_dma_project";    e = @{ EIDE_F1_DMA = "off"; EIDE_CAN_HALF = "tx" };   q = $true;  w = 2; fam = "f1" }
    @{ n = "F1 CAN RX only";               t = "emit_f1_dma_project";    e = @{ EIDE_F1_DMA = "off"; EIDE_CAN_HALF = "rx" };   q = $false; w = 2; fam = "f1" }
    @{ n = "F1 USB, both pads";            t = "emit_f1_dma_project";    e = @{ EIDE_F1_DMA = "off"; EIDE_USB = "both" };      q = $true; fam = "f1" }
    @{ n = "F1 USB, D- only";              t = "emit_f1_dma_project";    e = @{ EIDE_F1_DMA = "off"; EIDE_USB = "dm" };        q = $true; fam = "f1" }
    @{ n = "F1 USB, D+ only";              t = "emit_f1_dma_project";    e = @{ EIDE_F1_DMA = "off"; EIDE_USB = "dp" };        q = $false; fam = "f1" }
    @{ n = "F1 USB D- + GPIO on its pad";  t = "emit_f1_dma_project";    e = @{ EIDE_F1_DMA = "off"; EIDE_USB = "dm-gpio" };   q = $true; fam = "f1" }
    @{ n = "F1 every bus half-wired";      t = "emit_f1_dma_project";    e = @{ EIDE_F1_DMA = "both"; EIDE_USART_HALF = "rx"; EIDE_SPI_TXONLY = "1"; EIDE_I2C_HALF = "scl" }; q = $true; w = 3; fam = "f1" }
    # The F1 on embassy-stm32: every bus pin carries the AFIO remap, the timer
    # names its own, and the manifest is swapped by the same chain `app.rs`
    # runs. Linked, because 64 KiB of flash is the F103C8's real limit.
    @{ n = "F1 Async, default pads";       t = "emit_f1_async_project";  e = @{};                       q = $true; fam = "f1"; lk = $true }
    @{ n = "F1 Async, remapped, all DMA";  t = "emit_f1_async_project";  e = @{ EIDE_F1_ASYNC_REMAP = "1"; EIDE_F1_ASYNC_DMA = "1" }; q = $true; fam = "f1"; lk = $true }
    @{ n = "F1 Async -> Blocking switch";  t = "emit_f1_async_project";  e = @{ EIDE_F1_SWITCH = "back" }; q = $false; fam = "f1" }
    @{ n = "F1 RTIC";                      t = "emit_f1_rtic_project";   e = @{};                       q = $true; fam = "f1" }
    @{ n = "F1 Native";                    t = "emit_f1_native_project"; e = @{};                       q = $true; fam = "f1" }
    # The Configuration tab's flash store on an STM32: stm32f1xx-hal behind the
    # generated `F1Flash` adapter (Blocking, Native), embassy-stm32's `Flash` on
    # Async and on the other families, and memory.x's FLASH cut short by the
    # store. Linked, every one: only the linker reads memory.x and its ASSERT.
    @{ n = "F1 flash store";               t = "emit_stm32_store_project"; e = @{ EIDE_STORE_CHIP = "f103"; EIDE_STORE_RUNTIME = "blocking" }; q = $true;  fam = "f1"; lk = $true }
    @{ n = "F1 flash store, Native";       t = "emit_stm32_store_project"; e = @{ EIDE_STORE_CHIP = "f103"; EIDE_STORE_RUNTIME = "native" };   q = $false; fam = "f1"; lk = $true }
    @{ n = "F1 flash store, Async";        t = "emit_stm32_store_project"; e = @{ EIDE_STORE_CHIP = "f103"; EIDE_STORE_RUNTIME = "async" };    q = $false; fam = "f1"; lk = $true }

    # A different HAL and a different entry point, so a different set of ways to
    # be wrong: esp-hal bindings, and the esp-rtos scheduler on the async one.
    # BOTH arms name EIDE_ESP_RUNTIME, which is what `emit_esp32c3_project`
    # reads (codegen/embassy_common.rs). They used to set ESP_ASYNC_RUNTIME - a
    # real knob, but one belonging to two OTHER tests in codegen/family.rs - so
    # the harness saw neither, defaulted to blocking, and BOTH arms built the
    # blocking project. The async row was green and had never compiled the
    # esp-rtos path once; the hook subset below is that same row.
    #
    # The async arm now says so out loud instead of relying on a default, which
    # is what let the mistake hide.
    @{ n = "ESP32-C3 blocking";            t = "emit_esp32c3_project";       e = @{ EIDE_ESP_RUNTIME = "blocking" }; q = $true; fam = "esp" }
    @{ n = "ESP32-C3 async (esp-rtos)";    t = "emit_esp32c3_project";       e = @{ EIDE_ESP_RUNTIME = "async" }; q = $true; fam = "esp" }
    # The pull rides on the two runtimes that emit it. Two arms and not six:
    # `Pull::Up` and `Pull::Down` differ from `Pull::None` only in the variant
    # named, and the variants are checked by esp-hal - one of each per runtime
    # proves the CALL compiles, which is all this matrix can say.
    # A task lifted off the shared executor onto its own InterruptExecutor.
    # The unit tests assert on the emitted TEXT; only this compiles it, which is
    # what checks that `InterruptExecutor<1>`, `sw_int.software_interrupt1` and
    # `Priority::Priority2` are things esp-rtos and esp-hal actually have.
    @{ n = "ESP32-C3 preemptive task";     t = "emit_esp32c3_project";       e = @{ EIDE_ESP_RUNTIME = "async"; EIDE_ESP_IRQ = "rising"; EIDE_ESP_TASK_PRIO = "high" }; q = $true; fam = "esp" }
    @{ n = "ESP32-C3 input Pull::Up";      t = "emit_esp32c3_project";       e = @{ EIDE_ESP_RUNTIME = "blocking"; EIDE_ESP_PULL = "up" }; q = $true; fam = "esp" }
    @{ n = "ESP32-C3 async Pull::Down";    t = "emit_esp32c3_project";       e = @{ EIDE_ESP_RUNTIME = "async"; EIDE_ESP_PULL = "down" }; q = $true; fam = "esp" }
    # The harness wires ONE LEDC channel by default, so the two cases above only
    # ever reach the single-channel shape. Two channels is a different file: the
    # return type becomes a tuple, and the duty trait addresses it by POSITION,
    # which is not the channel number.
    @{ n = "ESP32-C3, two PWM channels";   t = "emit_esp32c3_project";       e = @{ EIDE_ESP_PWM = "0,2" }; q = $true; fam = "esp" }

    # The Configuration tab's flash store: esp-storage + sequential-storage
    # through the one template, on both runtimes (blocking wrappers vs async),
    # with the harness using verify/load/save as a user would. The async row
    # wires NO pin, the "select pins" default block that once dropped the
    # tab's init lines. The nvs mode writes no partitions.csv: full only.
    @{ n = "ESP32-C3 flash store";          t = "emit_esp32c3_project";       e = @{ EIDE_ESP_RUNTIME = "blocking"; EIDE_ESP_FLASHSTORE = "partition" }; q = $true; fam = "esp" }
    @{ n = "ESP32-C3 flash store, no pins"; t = "emit_esp32c3_project";       e = @{ EIDE_ESP_RUNTIME = "async"; EIDE_ESP_FLASHSTORE = "partition"; EIDE_ESP_NOPINS = "1" }; q = $true; fam = "esp" }
    @{ n = "ESP32-C3 flash store in nvs";   t = "emit_esp32c3_project";       e = @{ EIDE_ESP_RUNTIME = "blocking"; EIDE_ESP_FLASHSTORE = "nvs" }; q = $false; fam = "esp" }

    # The IoT tab: esp-radio 0.18 + esp-alloc + embassy-net + rust-mqtt + the
    # SNTP and ESP-NOW templates over the generated heap, spawner and
    # pins/configs/*.rs, on top of the harness's full wiring and watchdogs. The
    # loop calls every function the files offer, so the API is compiled, not
    # just declared. The C3 rows are the four shapes of main.rs - station +
    # MQTT WITHOUT the esp-now feature (no `radio.esp_now` field then), the
    # station alone, ESP-NOW alone (`hold_radio`), and everything; the other
    # chips build everything, each its own esp-radio and esp-wifi-sys blob
    # crate, and the Xtensa rows are where `alloc` in build-std matters.
    @{ n = "ESP32-C3 Wi-Fi + MQTT (IoT)";   t = "emit_esp32c3_project";       e = @{ EIDE_ESP_RUNTIME = "async"; EIDE_ESP_IOT = "mqtt" }; q = $true; fam = "esp" }
    @{ n = "ESP32-C3 IoT, everything on";   t = "emit_esp32c3_project";       e = @{ EIDE_ESP_RUNTIME = "async"; EIDE_ESP_IOT = "all" }; q = $true; fam = "esp" }
    @{ n = "ESP32-C3 ESP-NOW alone";        t = "emit_esp32c3_project";       e = @{ EIDE_ESP_RUNTIME = "async"; EIDE_ESP_IOT = "espnow" }; q = $true; fam = "esp" }
    @{ n = "ESP32-C3 Wi-Fi only (IoT)";     t = "emit_esp32c3_project";       e = @{ EIDE_ESP_RUNTIME = "async"; EIDE_ESP_IOT = "wifi" }; q = $false; fam = "esp" }
    @{ n = "ESP32 IoT, everything (Xtensa)";    t = "emit_esp32c3_project";   e = @{ EIDE_ESP_CHIP = "esp32";    EIDE_ESP_RUNTIME = "async"; EIDE_ESP_IOT = "all" }; q = $false; fam = "esp" }
    @{ n = "ESP32 ESP-NOW alone (Xtensa)";      t = "emit_esp32c3_project";   e = @{ EIDE_ESP_CHIP = "esp32";    EIDE_ESP_RUNTIME = "async"; EIDE_ESP_IOT = "espnow" }; q = $false; fam = "esp" }
    @{ n = "ESP32-S2 IoT, everything (Xtensa)"; t = "emit_esp32c3_project";   e = @{ EIDE_ESP_CHIP = "esp32s2";  EIDE_ESP_RUNTIME = "async"; EIDE_ESP_IOT = "all" }; q = $false; fam = "esp" }
    @{ n = "ESP32-S3 IoT, everything (Xtensa)"; t = "emit_esp32c3_project";   e = @{ EIDE_ESP_CHIP = "esp32s3";  EIDE_ESP_RUNTIME = "async"; EIDE_ESP_IOT = "all" }; q = $false; fam = "esp" }
    @{ n = "ESP32-C2 IoT, everything";      t = "emit_esp32c3_project";       e = @{ EIDE_ESP_CHIP = "esp32c2";  EIDE_ESP_RUNTIME = "async"; EIDE_ESP_IOT = "all" }; q = $false; fam = "esp" }
    @{ n = "ESP32-C5 IoT, everything";      t = "emit_esp32c3_project";       e = @{ EIDE_ESP_CHIP = "esp32c5";  EIDE_ESP_RUNTIME = "async"; EIDE_ESP_IOT = "all" }; q = $false; fam = "esp" }
    @{ n = "ESP32-C6 IoT, everything";      t = "emit_esp32c3_project";       e = @{ EIDE_ESP_CHIP = "esp32c6";  EIDE_ESP_RUNTIME = "async"; EIDE_ESP_IOT = "all" }; q = $false; fam = "esp" }
    @{ n = "ESP32-C61 IoT, everything";     t = "emit_esp32c3_project";       e = @{ EIDE_ESP_CHIP = "esp32c61"; EIDE_ESP_RUNTIME = "async"; EIDE_ESP_IOT = "all" }; q = $false; fam = "esp" }

    # The watchdogs on EVERY bundled Espressif part. The harness switches all
    # three on unless EIDE_ESP_WDG=0, so the six C3 rows above already build
    # them on both runtimes - including async, where the scheduler owns TIMG0.
    # These eight are the other chips: each is its own esp-hal build, so a C3
    # pass says nothing about them. The C2 has no TIMG1 and its row asks for
    # MWDT1 anyway: it compiles only if the generator dropped it.
    # Full-only: eight esp-hal builds, three on the Xtensa toolchain.
    @{ n = "ESP32 watchdogs (Xtensa)";     t = "emit_esp32c3_project";       e = @{ EIDE_ESP_CHIP = "esp32";    EIDE_ESP_RUNTIME = "blocking" }; q = $false; fam = "esp" }
    @{ n = "ESP32-S2 watchdogs (Xtensa)";  t = "emit_esp32c3_project";       e = @{ EIDE_ESP_CHIP = "esp32s2";  EIDE_ESP_RUNTIME = "blocking" }; q = $false; fam = "esp" }
    @{ n = "ESP32-S3 watchdogs (Xtensa)";  t = "emit_esp32c3_project";       e = @{ EIDE_ESP_CHIP = "esp32s3";  EIDE_ESP_RUNTIME = "blocking" }; q = $false; fam = "esp" }
    @{ n = "ESP32-C2 watchdogs, no TIMG1"; t = "emit_esp32c3_project";       e = @{ EIDE_ESP_CHIP = "esp32c2";  EIDE_ESP_RUNTIME = "blocking" }; q = $false; fam = "esp" }
    @{ n = "ESP32-C5 watchdogs";           t = "emit_esp32c3_project";       e = @{ EIDE_ESP_CHIP = "esp32c5";  EIDE_ESP_RUNTIME = "blocking" }; q = $false; fam = "esp" }
    @{ n = "ESP32-C6 watchdogs";           t = "emit_esp32c3_project";       e = @{ EIDE_ESP_CHIP = "esp32c6";  EIDE_ESP_RUNTIME = "blocking" }; q = $false; fam = "esp" }
    @{ n = "ESP32-C61 watchdogs";          t = "emit_esp32c3_project";       e = @{ EIDE_ESP_CHIP = "esp32c61"; EIDE_ESP_RUNTIME = "blocking" }; q = $false; fam = "esp" }
    @{ n = "ESP32-H2 watchdogs";           t = "emit_esp32c3_project";       e = @{ EIDE_ESP_CHIP = "esp32h2";  EIDE_ESP_RUNTIME = "blocking" }; q = $false; fam = "esp" }

    # A third HAL family, and the first that is not STM32 or Espressif. One
    # harness, FOUR boards: Pico and Pico W on thumbv6m, Pico 2 and Pico 2 W on
    # thumbv8m, each printing its own `target:`.
    #
    # The W boards are not a formality: GP23/24/25/29 belong to the CYW43 radio
    # there, so the emitter has to leave them alone — including GP25, which is
    # the LED on every other board in the set.
    #
    # Worth its place because almost every API in that backend was written from
    # documentation, and the compiler has already caught two of them - `.freq()`
    # and `set_duty_cycle` both live on embedded-hal traits that have to be
    # imported, and neither failure is visible by reading.
    #
    # Each board also carries the watchdog at its driver's LAST accepted period,
    # so the `const` assert in watchdog.rs is compiled at its boundary, and the
    # 1 us tick main.rs starts is compiled at each HAL's width (u8 / u16).
    @{ n = "Raspberry Pi Pico x4";         t = "emit_rp_project";            e = @{};                       q = $true; fam = "rp"; lk = $true }

    # The micro:bit on nrf52833-hal, TWO projects: every peripheral wired on the
    # default branches, and a second on the other ones (crystal HFCLK,
    # synthesized LFCLK, open-drain, pull-down, CTS/RTS, SCK-only SPI in mode 3,
    # PWM with no frequency). Same reason as the Pico row: the HAL calls were
    # read from nrf-hal-common's source, and the type-state on `Clocks` plus
    # the degraded-vs-typed pin split are things only a compiler settles.
    @{ n = "BBC micro:bit v2 x2";          t = "emit_nrf_project";           e = @{};                       q = $true; fam = "nrf" }

    # The same board on embassy-nrf, TWO projects again: every peripheral on
    # the default branches, and one on the others (crystal HFCLK, synthesized
    # LFCLK, armed pull-up and pull-down inputs, open-drain, an NFC pad,
    # CTS/RTS, a TX-only SPIM in mode 3 LSB first, TWIM1, a center-aligned
    # active-low open-drain PWM on a channel that is not its slot, and a PWM
    # with no frequency). Both projects are reached through a runtime SWITCH
    # from the blocking one, the path a user takes: main.rs keeps the header
    # the two runtimes share, and Cargo.toml has its HAL crate swapped.
    @{ n = "BBC micro:bit v2 async x2";    t = "emit_nrf_async_project";     e = @{};                       q = $true; fam = "nrf" }

    # The rest of the nRF52 family, TEN projects on two targets: Nordic's two
    # kits on both runtimes, and the small parts on the nRF52 DK the way the
    # New MCU form retargets a definition. Each part is here for what it does
    # differently - the 52810 has SPIM0 and TWIM0 as SEPARATE blocks, the 52811
    # shares TWIM0 with SPIM1, the 52805 has no PWM and only AIN2/AIN3, and the
    # 52820 has no nrf-hal crate at all, so its Blocking project is embassy-nrf
    # without an executor. All of them wire the blocks the part LACKS too: those
    # must come out as comments, and only a compiler proves none slipped through.
    # Quick runs one per target plus the 52820's Blocking, the only emitter here
    # nothing else exercises.
    # The nRF5340 DK joins it, a third target (thumbv8m.main): the application
    # core's SERIALn blocks, SPIM4, WDT0 and the USB regulator's VBUS vector.
    # Then the nRF54L15 DK: SERIAL00/2x/30, PWM20, the GRTC, port 2, and WDT0
    # on the secure core - the one name the compiler caught (`WDT` is `_ns`).
    #
    # All fourteen in quick mode and in the hook. It had an `only` subset of
    # five while every run rebuilt every dependency (601 s - the harness used
    # to delete `target/` along with the project); with the cache kept the row
    # costs ~70 s warm, and the micro:bit rows alone told the hook nothing
    # about the other eight parts.
    @{ n = "nRF52 + nRF5340 + nRF54L15 x14"; t = "emit_nrf_family_projects"; e = @{};                     q = $true; fam = "nrf" }

    # The same two boards on embassy-rp, which is a DIFFERENT HAL crate, not a
    # feature of the first one. Every bus is wired, because that is where the
    # compiler found the two things reading could not: a DMA channel needs its
    # OWN handler bound on DMA_IRQ_0, and `Spi::new` wants the binding AFTER
    # its channels while `Uart::new` wants it before.
    #
    # THREE projects, not two: both boards on the buffered uart, plus a third
    # that forces the DMA transport. Those two build different types, bind a
    # different interrupt handler on the same vector, and renumber every later
    # peripheral - one of them alone proves nothing about the other.
    #
    # The count in the name is load-bearing. It read `x2` for a while after the
    # third project arrived, and a name that understates its own coverage is how
    # a gap hides in plain sight - the same way `23 of 23` hid six unrun cases.
    #
    # The watchdog rides here too, at embassy-rp's own ceiling - twice the
    # Blocking one on the RP2350. It is the first config file this backend ever
    # wrote, so the harness now writes what `config_files` returns instead of
    # an empty `configs/mod.rs`.
    @{ n = "Raspberry Pi Pico async x3";   t = "emit_rp_async_project";      e = @{};                       q = $true; fam = "rp"; lk = $true }


    # The two W boards, whose on-board LED is not on the chip at all - it is
    # GPIO0 of the CYW43 radio, reached through a PIO-driven half-duplex SPI and
    # an async-only driver. `write_project` lays down the real firmware the IDE
    # ships, and the harness checks the SIZES, so a stub cannot pass for it.
    @{ n = "Raspberry Pi Pico W radio x2"; t = "emit_rp_radio_project";      e = @{};                       q = $true; fam = "rp"; lk = $true }

    # The IoT tab on both W boards: the radio up once for the LED AND the
    # network, `control` handed to the Wi-Fi task, embassy-net + rust-mqtt
    # (bump buffer, no heap) + SNTP over it. Linked, like the radio row.
    @{ n = "Raspberry Pi Pico W Wi-Fi + MQTT + SNTP x2"; t = "emit_rp_iot_project"; e = @{};                q = $true; fam = "rp"; lk = $true }

    # The pico2-ice, Blocking and Async: the only RP2350B board, so the only
    # rows of the FUNCSEL table past GP29 (UART1 on GP36/37, SPI0 on 32/34/35,
    # PWM slice 11) and the only project on embassy-rp's `rp235xb` feature.
    # Both carry the FPGA loader and the 104 KB bitstream it includes, which is
    # exactly the image size that pushed IMAGE_DEF out of the boot ROM's 4 KiB
    # before memory.x placed it - so `lk` is what proves this row.
    @{ n = "pico2-ice x2";                 t = "emit_pico2_ice_project";     e = @{};                       q = $true; fam = "rp"; lk = $true }

    # ONE test, NINE projects, four targets — GPIO, async, USART, DMA on F4/F2/F7,
    # the watchdogs and WBA. Each prints its own `target:`, so they are paired
    # individually rather than forced onto one triple.
    # 346s of the quick set's 933s came from THIS ONE case — nine projects, nine
    # dependency graphs, nothing shared. `only` names the four that quick mode
    # cross-compiles; `-Full` still builds all nine.
    #
    # Chosen as one project per TARGET, plus one per thing no other project in
    # the quick set exercises:
    #   _dma      thumbv7em — the richest wiring (DMA + every bus)
    #   _async    thumbv7em — the async runtime, which is a different emitter
    #   _dma_f2   thumbv7m  — F2's own PLL floor, a documented trap
    #   wba_wdg   thumbv8m  — a different family AND the watchdog arithmetic
    # Dropped from quick: the base GPIO project, _usart, _dma_f7 and the F4
    # watchdog (all thumbv7em, all shapes the four above already cover), and
    # eide_f1_check_usart — F1 already has thirteen cases of its own here.
    #
    # The harness still WRITES all nine: writing is free, `cargo check` is not,
    # and a harness that emits less under a flag is a harness that can rot.
    @{ n = "embassy (9 projects)";         t = "emit_embassy_project";       e = @{};                       q = $true; fam = "embassy"
       only = @("eide_embassy_check_dma", "eide_embassy_check_async", "eide_embassy_check_dma_f2", "eide_wba_check_wdg") }
    # The flash store on embassy-stm32: 2 KiB pages and 8-byte words (G431),
    # 8 KiB pages and 16-byte words on a v8-M part (WBA55), and the Async
    # runtime on the G431. Linked - see the F1 rows.
    @{ n = "STM32 flash store, G431";      t = "emit_stm32_store_project"; e = @{ EIDE_STORE_CHIP = "g431"; EIDE_STORE_RUNTIME = "blocking" }; q = $true;  fam = "embassy"; lk = $true }
    @{ n = "STM32 flash store, WBA55";     t = "emit_stm32_store_project"; e = @{ EIDE_STORE_CHIP = "wba55"; EIDE_STORE_RUNTIME = "blocking" }; q = $false; fam = "embassy"; lk = $true }
    @{ n = "STM32 flash store, G431 Async"; t = "emit_stm32_store_project"; e = @{ EIDE_STORE_CHIP = "g431"; EIDE_STORE_RUNTIME = "async" };   q = $false; fam = "embassy"; lk = $true }
    # F4/F7 end in 128/256 KiB sectors: the store sits in the small ones right
    # after the vector table, through embassy's first flash region, and
    # memory.x starts the program after it (`_stext`).
    @{ n = "STM32 flash store, F411";       t = "emit_stm32_store_project"; e = @{ EIDE_STORE_CHIP = "f411"; EIDE_STORE_RUNTIME = "blocking" }; q = $true;  fam = "embassy"; lk = $true }
    @{ n = "STM32 flash store, F411 Async"; t = "emit_stm32_store_project"; e = @{ EIDE_STORE_CHIP = "f411"; EIDE_STORE_RUNTIME = "async" };   q = $false; fam = "embassy"; lk = $true }
    @{ n = "STM32 flash store, F746";       t = "emit_stm32_store_project"; e = @{ EIDE_STORE_CHIP = "f746"; EIDE_STORE_RUNTIME = "blocking" }; q = $false; fam = "embassy"; lk = $true }

    # These two build from a REAL part in the vendor database rather than from a
    # bundled definition, which is the only way to exercise the importer's own
    # output — channel names, interrupt names, the `bind_interrupts!` grouping.
    # `p` is what they need; without it they are skipped, not failed, because a
    # machine without the database is a normal machine.
    @{ n = "imported chip, async DMA";     t = "emit_imported_dma_project";  e = @{}; q = $true; p = $CUBE_DB; fam = "import" }
    @{ n = "imported chip, comparators";   t = "emit_comp_project";          e = @{}; q = $true; p = $CUBE_DB; fam = "import" }

    # STM32N6 — the first family whose clock block does not come from the
    # descriptor model. Cortex-M55 on a v8-M Main triple, a chip feature derived
    # through the `x`+suffix rule, and an RCC block with four-PLL types in it:
    # three separate derivations that were each wrong until this project was
    # emitted end to end.
    @{ n = "STM32N6 clock + project";      t = "emit_n6_project";            e = @{}; q = $true; p = $CUBE_DB; fam = "n6" }

    # Not a project: a VERDICT (`v`). STM32WL30 is the chip the import preflight
    # was written for — `embassy-stm32` publishes no `stm32wl3*` feature, its
    # clock tree is an architecture no recipe can read, and its DMA channels
    # only appeared once `parse_value` started reading the vendor's own range.
    # It cannot be cross-compiled BECAUSE of the first of those, so what is
    # pinned here is the verdict itself, with G071 alongside as the control.
    # This case fails the day a `stm32wl3` recipe lands and the answer has to
    # change — which is the only way anyone would remember to change it.
    @{ n = "WL30 preflight verdict";       t = "wl30_is_the_chip_this_preflight_exists_for"; e = @{}; q = $true; p = $CUBE_DB; v = $true; fam = "import" }
)

# Every knob any case sets, so one case cannot leak into the next.
# EIDE_ESP_RUNTIME, EIDE_ESP_IRQ and EIDE_ESP_CHIP were missing, so a case
# that set one left it set for every case after it. ESP_ASYNC_RUNTIME stays:
# `write_esp_dma_project` and `emit_esp_periph_project` in codegen/family.rs
# read it, and a stale value there would pick the wrong runtime just as quietly.
$KNOBS = @("EIDE_F1_DMA", "EIDE_SPI_TXONLY", "EIDE_USART_HALF", "EIDE_I2C_HALF",
           "EIDE_CAN_HALF", "EIDE_USB", "EIDE_F1_ASYNC_REMAP", "EIDE_F1_ASYNC_DMA",
           "EIDE_F1_SWITCH", "ESP_ASYNC_RUNTIME",
           "EIDE_ESP_PWM", "EIDE_ESP_RUNTIME", "EIDE_ESP_IRQ", "EIDE_ESP_CHIP",
           "EIDE_ESP_PULL", "EIDE_ESP_WDG", "EIDE_ESP_FLASHSTORE", "EIDE_ESP_NOPINS",
           "EIDE_ESP_TASK_PRIO", "EIDE_STORE_CHIP", "EIDE_STORE_RUNTIME",
           "EIDE_ESP_IOT")

# The hook passes its families as ONE comma-joined argument ("embassy,f1"), and
# `powershell -File` binds that to [string[]] as a single element - it does not
# split on commas the way an interactive call does. Unsplit, it was an "unknown
# family", exit 2, and every push touching two families was refused. Split
# here, so both `-Hook a,b` and `-Hook "a,b"` mean the same.
$Hook = @($Hook | ForEach-Object { $_ -split ',' } | ForEach-Object { $_.Trim() } | Where-Object { $_ })

if ($Hook.Count -gt 0) {
    $known = $ALL_CASES | ForEach-Object { $_.fam } | Sort-Object -Unique
    $bad = $Hook | Where-Object { $known -notcontains $_ }
    if ($bad) {
        Write-Host ("unknown family: {0}  (known: {1})" -f ($bad -join ", "), ($known -join ", ")) -ForegroundColor Red
        exit 2
    }
    # `@(...)` on every branch: a pipeline that matches exactly ONE case yields
    # the hashtable itself, not an array, and `.Count` on a hashtable is its
    # number of KEYS. That printed "7 of 30" for a single-case run.
    $cases = @($ALL_CASES | Where-Object { $Hook -contains $_.fam })
} elseif ($Full) {
    $cases = @($ALL_CASES)
} else {
    $cases = @($ALL_CASES | Where-Object { $_.q })
}
Write-Host ("running {0} of {1} cases{2}" -f $cases.Count, $ALL_CASES.Count,
    $(if ($Hook.Count -gt 0) { "  (hook subset: " + ($Hook -join ", ") + ")" }
      elseif ($Full) { "" } else { "  (use -Full for all)" }))
Write-Host ""

$results = @()
foreach ($c in $cases) {
    if ($c.p -and -not (Test-Path $c.p)) {
        $results += [pscustomobject]@{ Case = $c.n; Status = "skipped"; Detail = "no vendor database at $($c.p)" }
        Write-Host ("  {0,-34} skipped (no database)" -f $c.n) -ForegroundColor DarkGray
        continue
    }
    foreach ($k in $KNOBS) { Remove-Item ("Env:\" + $k) -ErrorAction SilentlyContinue }
    foreach ($k in $c.e.Keys) { Set-Item ("Env:\" + $k) $c.e[$k] }
    $sw = [System.Diagnostics.Stopwatch]::StartNew()

    Set-Location $repo
    $out = cargo test --bins $c.t -- --ignored --nocapture 2>&1
    # The test binary itself did not build - an unfinished edit elsewhere in
    # the repo, typically. Without this it fell through to "NO SUCH TEST",
    # which sends whoever reads it hunting for a renamed harness: on
    # 2026-10-02 the last two cases of a run "lost" their tests to a half-done
    # struct field in src/lsp.rs while the rest of the matrix was green.
    if ($out | Select-String -Pattern "^error: could not compile") {
        $why = $out | Select-String -Pattern "^error(\[E\d+\])?:" | Select-Object -First 1
        $results += [pscustomobject]@{ Case = $c.n; Status = "HARNESS COMPILE FAILED"; Detail = "$($why.Line.Trim()) - is someone editing the repo?" }
        Write-Host ("  {0,-34} HARNESS COMPILE FAILED" -f $c.n) -ForegroundColor Red
        continue
    }
    if ($out | Select-String -Pattern "panicked at|test result: FAILED") {
        $results += [pscustomobject]@{ Case = $c.n; Status = "EMIT FAILED"; Detail = "the harness's own assertions" }
        continue
    }
    # A filter that matches nothing is a SUCCESSFUL cargo run of zero tests, so
    # a typo in the test name would otherwise read as "the harness printed
    # nothing" — a wrong diagnosis pointing at the wrong file.
    if (-not ($out | Select-String -Pattern "test result: ok\. [1-9]")) {
        $results += [pscustomobject]@{ Case = $c.n; Status = "NO SUCH TEST"; Detail = "cargo ran 0 tests for filter '$($c.t)'" }
        continue
    }

    # A verdict case has nothing to build: the two checks above — the test ran,
    # and it did not fail — ARE the case. Everything below is about projects.
    if ($c.v) {
        $results += [pscustomobject]@{ Case = $c.n; Status = "ok (verdict)"; Detail = ""; Seconds = $sw.Elapsed.TotalSeconds }
        Write-Host ("  {0,-34} {1,-22} {2,5:N0}s" -f $c.n, "ok (verdict)", $sw.Elapsed.TotalSeconds) -ForegroundColor Green
        continue
    }

    # The harness says where it wrote and what to build it for; trusting those
    # lines is what keeps this script from duplicating the directory table.
    #
    # Three shapes exist today and all three are accepted, because normalising
    # them means editing a dozen tests to fix a script:
    #   wrote <path>                    F1, ESP — followed by its own `target:`
    #   wrote <path> (Display Name)     the chip-database harnesses
    #   wrote <path>  …no target line   the embassy harness, several projects
    # A `target:` line applies to the `wrote` above it, so a harness emitting
    # several projects for several targets pairs up correctly. `t2` on the case
    # is the fallback for the ones that print none.
    $projects = @()
    foreach ($line in $out) {
        $l = "$line"
        if ($l -match "^wrote (\S+)") {
            $projects += [pscustomobject]@{ Dir = $matches[1]; Target = $c.t2 }
        } elseif ($l -match "^target: (\S+)" -and $projects.Count -gt 0) {
            $projects[-1].Target = $matches[1]
        }
    }
    $projects = @($projects | Where-Object { $_.Dir -and (Test-Path $_.Dir) })
    # What this run touched. Anything ELSE under the work root is left over from
    # a case that has since been renamed or removed, and is pure waste: 39
    # directories had accumulated where a run uses 26.
    foreach ($pr in $projects) { $script:touched[(Split-Path $pr.Dir -Leaf)] = $true }
    if (-not $projects) {
        $results += [pscustomobject]@{ Case = $c.n; Status = "NO OUTPUT"; Detail = "harness printed no usable 'wrote' line" }
        continue
    }
    if ($projects | Where-Object { -not $_.Target }) {
        $results += [pscustomobject]@{ Case = $c.n; Status = "NO TARGET"; Detail = "harness printed no 'target:' and the case declares no t2" }
        continue
    }

    # Quick mode may check only some of what a multi-project harness wrote.
    $wrote = $projects.Count
    # Not under -Hook either: that mode promises EVERY case of a family, and the
    # projects outside `only` are the ones a narrowed push would otherwise never
    # compile - the embassy harness's F1 project among them.
    if ($c.only -and -not $Full -and $Hook.Count -eq 0) {
        $projects = @($projects | Where-Object { $c.only -contains (Split-Path $_.Dir -Leaf) })
        # A name that matches nothing would SHRINK the case silently and still
        # report ok — the same shape as the "filter matched no tests" bug this
        # script already guards against, so it gets the same treatment.
        if ($projects.Count -ne $c.only.Count) {
            $results += [pscustomobject]@{ Case = $c.n; Status = "BAD SUBSET"
                Detail = "only lists $($c.only.Count) project(s), $($projects.Count) matched what the harness wrote"
                Seconds = $sw.Elapsed.TotalSeconds }
            Write-Host ("  {0,-34} BAD SUBSET" -f $c.n) -ForegroundColor Red
            continue
        }
    }

    $status = "ok"
    $detail = ""
    $seen = 0
    foreach ($p in $projects) {
        Set-Location $p.Dir
        $r = if ($c.lk) { cargo build --release --target $p.Target 2>&1 }
             else { cargo check --target $p.Target 2>&1 }
        $errs = @($r | Select-String -Pattern "^error(\[|:)").Count
        # Cargo's future-incompatibility notice is about a DEPENDENCY, not about
        # the code being checked - `rp235x-hal` pulls in a proc-macro crate that
        # carries one. Counting it made a case fail for something no generator
        # can fix, and declaring it "expected" would be worse: the allowance
        # would go stale the day that crate is updated, and the matrix compares
        # warning counts in BOTH directions.
        $w = @($r | Select-String -Pattern "^warning: " |
            Where-Object { $_.Line -notmatch "will be rejected by a future version of Rust" })
        $seen += $w.Count
        if ($errs -gt 0) {
            $status = "$errs ERRORS"
            # A failed link says only "linking with `rust-lld` failed"; the
            # reason - a memory.x ASSERT, say - is on the linker's own line.
            $why = $r | Select-String -Pattern "rust-lld: error" | Select-Object -First 1
            if (-not $why) { $why = $r | Select-String -Pattern "^error(\[|:)" | Select-Object -First 1 }
            $detail = $why.Line.Trim()
            break
        }
        if ($w.Count -gt 0 -and -not $detail) { $detail = $w[0].Line.Trim() }
    }
    $allowed = if ($null -ne $c.w) { [int]$c.w } else { 0 }
    if ($status -eq "ok" -and $seen -ne $allowed) {
        $status = "$seen warn, expected $allowed"
        if ($seen -lt $allowed) { $detail = "fewer warnings than declared - lower `w` on this case" }
    } elseif ($status -eq "ok" -and $seen -gt 0) {
        $status = "ok ($seen expected)"
    }
    if ($projects.Count -lt $wrote) {
        $status = "$status, $($projects.Count) of $wrote"
    }
    $results += [pscustomobject]@{ Case = $c.n; Status = $status; Detail = $detail; Seconds = $sw.Elapsed.TotalSeconds }
    $colour = if ($status -like "*ERROR*") { "Red" } elseif ($status -like "*warn*") { "Yellow" } else { "Green" }
    Write-Host ("  {0,-34} {1,-22} {2,5:N0}s" -f $c.n, $status, $sw.Elapsed.TotalSeconds) -ForegroundColor $colour
}

Set-Location $repo
foreach ($k in $KNOBS) { Remove-Item ("Env:\" + $k) -ErrorAction SilentlyContinue }

Write-Host ""
$bad = @($results | Where-Object {
    $_.Status -like "*ERROR*" -or $_.Status -like "*FAILED*" -or
    $_.Status -like "NO *" -or $_.Status -like "*expected*" -and $_.Status -notlike "ok *"
})
if ($bad) {
    Write-Host "FAILED:" -ForegroundColor Red
    $bad | ForEach-Object { Write-Host ("  {0}: {1}`n      {2}" -f $_.Case, $_.Status, $_.Detail) -ForegroundColor Red }
    $env:TMP = $script:origTmp
    $env:TEMP = $script:origTemp
    exit 1
}
$skipped = @($results | Where-Object { $_.Status -eq "skipped" })
$ran = $results.Count - $skipped.Count
$timed = @($results | Where-Object { $_.Seconds })
if ($timed) {
    $total = ($timed | Measure-Object -Property Seconds -Sum).Sum
    $worst = $timed | Sort-Object -Property Seconds -Descending | Select-Object -First 3
    Write-Host ("{0:N0}s total; slowest: {1}" -f $total,
        (($worst | ForEach-Object { "{0} {1:N0}s" -f $_.Case, $_.Seconds }) -join ", ")) -ForegroundColor DarkGray
}
Write-Host ("all {0} cases pass{1}" -f $ran,
    $(if ($skipped) { " ($($skipped.Count) skipped)" } else { "" })) -ForegroundColor Green
# "All 0 pass" is not a pass. A family whose only case needs the vendor
# database (n6) verifies nothing on a machine without it, and saying so is the
# least a gate can do. Not a failure: blocking a push over a missing optional
# database would be worse than the gap.
if ($ran -eq 0) {
    Write-Host "NOTHING WAS VERIFIED - every selected case was skipped (no vendor database?)" -ForegroundColor Yellow
}

# Drop what no longer belongs, and ONLY then.
#
# Not "delete everything at the end": the `target/` inside each project is what
# keeps a run at ~24 minutes instead of recompiling every dependency, so wiping
# them would make the matrix something people avoid running. What IS waste is a
# directory no current case writes - left by a case since renamed or removed.
#
# Three guards, each for a different way this could delete something wanted:
#   - a FAILED run may not have reached the cases that write the rest;
#   - a SKIPPED case (no vendor database) writes nothing but is not gone;
#   - a -Hook subset touches a handful of directories on purpose;
#   - so does the QUICK set: it never runs the -Full-only cases, so their
#     directories look stale to it and their warm caches would go. That stayed
#     unseen while only manual runs had a work root; once the hook exported
#     EIDE_MATRIX_DIR, every quick-set push would have emptied them.
# Any of them, and the prune is the wrong answer, so it does not run.
if ($script:workRoot -and $Full -and $Hook.Count -eq 0 -and -not $skipped) {
    $stale = @(Get-ChildItem $script:workRoot -Directory -Filter "eide_*" -ErrorAction SilentlyContinue |
        Where-Object { -not $script:touched.ContainsKey($_.Name) })
    if ($stale) {
        $freed = 0
        foreach ($d in $stale) {
            $freed += (Get-ChildItem $d.FullName -Recurse -File -ErrorAction SilentlyContinue |
                Measure-Object Length -Sum).Sum
            Remove-Item $d.FullName -Recurse -Force -ErrorAction SilentlyContinue
        }
        Write-Host ("pruned {0} stale project dir(s), {1:N1} GB - none of them written by this run" -f
            $stale.Count, ($freed / 1GB)) -ForegroundColor DarkGray
    }
    $left = (Get-ChildItem $script:workRoot -Recurse -File -ErrorAction SilentlyContinue |
        Measure-Object Length -Sum).Sum
    Write-Host ("{0} holds {1:N1} GB of warm build caches" -f $script:workRoot, ($left / 1GB)) -ForegroundColor DarkGray
}
$env:TMP = $script:origTmp
$env:TEMP = $script:origTemp
exit 0
