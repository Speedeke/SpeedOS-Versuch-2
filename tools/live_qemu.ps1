# tools/live_qemu.ps1 -- Startet das USB-LIVE-Image in QEMU (UEFI/OVMF)
#
# Damit probiert man speedos-live.img in der virtuellen Maschine, BEVOR
# man es auf einen echten USB-Stick schreibt (die "Generalprobe" aus
# docs/usb-boot.md). Anders als `cargo run` haengt dieses Skript KEINE
# Daten-/FAT-Platte an -- es bootet nur das Live-Image, genau wie es auf
# echter Hardware laeuft (ohne Platte faellt SpeedOS sauber auf das
# RAM-Dateisystem zurueck).
#
# ASCII-only mit Absicht: Windows PowerShell 5.1 liest .ps1 ohne BOM als
# ANSI und wuerde Umlaute/Gedankenstriche als Steuerzeichen missdeuten.
#
# Beispiele:
#   .\tools\live_qemu.ps1                      # 1280x720, Desktop
#   .\tools\live_qemu.ps1 -Breite 1920 -Hoehe 1080
#   .\tools\live_qemu.ps1 -KeinePS2            # kein PS/2 -> Eingabe-Meldung
#   .\tools\live_qemu.ps1 -Qmp 4444            # QMP-Fernsteuerung (Screenshots)
#
# Beenden: das QEMU-Fenster schliessen oder im Terminal Strg+C.

param(
    [string]$Image   = "speedos-live.img",  # das zu bootende Live-Image
    [int]$Breite     = 1280,                # gewuenschte Breite (GOP)
    [int]$Hoehe      = 720,                 # gewuenschte Hoehe
    [int]$Ram        = 0,                   # RAM in MiB (0 = automatisch)
    [switch]$KeinePS2,                      # PS/2-Controller ganz abschalten
    [switch]$Kopflos,                       # ohne Fenster (-display none)
    [int]$Qmp        = 0                    # QMP-Port (0 = aus); fuer Screenshots
)

$ErrorActionPreference = "Stop"

# --- QEMU + UEFI-Firmware finden (wie im Runner boot/src/main.rs) ---
$qemu = "qemu-system-x86_64.exe"
if (-not (Get-Command $qemu -ErrorAction SilentlyContinue)) {
    $qemu = "C:\Program Files\qemu\qemu-system-x86_64.exe"
}
$firmware    = "C:\Program Files\qemu\share\edk2-x86_64-code.fd"
$varsVorlage = "C:\Program Files\qemu\share\edk2-i386-vars.fd"
if (-not (Test-Path $Image))    { throw "Image nicht gefunden: $Image  (zuerst 'cargo image' ausfuehren)" }
if (-not (Test-Path $firmware)) { throw "UEFI-Firmware nicht gefunden: $firmware" }

# Jede VM braucht ihre EIGENE, beschreibbare Kopie der NVRAM-Variablen.
$varsPfad = "speedos-live.vars.fd"
Copy-Item $varsVorlage $varsPfad -Force

# --- VRAM + RAM passend zur Aufloesung ableiten (wie im Runner) ---
# Das VRAM (Zweierpotenz) ist der eigentliche Aufloesungs-Waehler: die
# Firmware bietet nur Modi an, die hineinpassen.
$bedarf = [int64]$Breite * [int64]$Hoehe * 4
$vgamem = 16
while ($vgamem * 1MB -lt $bedarf) { $vgamem *= 2 }
if ($vgamem -gt 256) { $vgamem = 256 }

if ($Ram -le 0) {
    $Ram = [int]([math]::Floor(([int64]$Breite * $Hoehe * 20) / 1MB) + 96)
    $Ram = [int]([math]::Ceiling($Ram / 64) * 64)
    if ($Ram -lt 128)  { $Ram = 128 }
    if ($Ram -gt 1024) { $Ram = 1024 }
}

# --- QEMU-Argumente zusammenbauen ($qargs, NICHT $args -- das ist eine
#     automatische PowerShell-Variable) ---
$qargs = @(
    # UEFI-Firmware: Code readonly, NVRAM als beschreibbare Kopie.
    "-drive", "if=pflash,format=raw,readonly=on,file=$firmware",
    "-drive", "if=pflash,format=raw,file=$varsPfad",
    # DAS Live-Image als normale Platte (die Boot-Platte):
    "-drive", "format=raw,file=$Image",
    # Hardware-Virtualisierung (WHPX), TCG als Fallback:
    "-accel", "whpx,kernel-irqchip=off",
    "-accel", "tcg",
    "-m",     "${Ram}M",
    "-rtc",   "base=localtime",
    # Grafik: reine VGA mit gewuenschter Aufloesung (VRAM = Waehler).
    "-vga",   "none",
    "-device","VGA,edid=on,xres=$Breite,yres=$Hoehe,vgamem_mb=$vgamem",
    # Serielle Ausgabe ins Terminal (auf echter Hardware gibt es die
    # NICHT -- dafuer der Diagnose-Modus mit Taste D).
    "-serial","stdio"
)

if ($KeinePS2) {
    # QEMU kann NUR die Maus nicht gezielt weglassen; i8042=off schaltet
    # den GESAMTEN PS/2-Controller ab (Tastatur UND Maus) -- genau der
    # Fall, den die "keine PS/2-Eingabe"-Meldung abfangen soll.
    $qargs += @("-machine", "pc,i8042=off")
    Write-Host "[live_qemu] i8042=off -- kein PS/2-Controller (Test der Eingabe-Meldung)."
}
if ($Qmp -gt 0)  { $qargs += @("-qmp", "tcp:127.0.0.1:$Qmp,server,nowait") }
if ($Kopflos)    { $qargs += @("-display", "none") }

Write-Host "[live_qemu] $Breite x $Hoehe, VRAM $vgamem MiB, RAM $Ram MiB -- starte QEMU ..."
& $qemu @qargs
