# Migrationsplan: bootloader 0.9 → 0.11 (bootloader_api 0.11.15)

Stand: Juli 2026, Branch `bootloader-migration`.
Quelle: Quellcode von `bootloader_api 0.11.15` und `bootloader 0.11.15`
aus der Cargo-Registry (API direkt verifiziert, nicht nur Doku gelesen).

## Warum überhaupt?

bootloader 0.9 bootet in den VGA-**Textmodus** — einen Grafikmodus kann
man aus dem 64-Bit-Modus praktisch nicht mehr schalten (bräuchte
BIOS-Aufrufe). bootloader 0.11 richtet **vor** dem Sprung in den Kernel
per VESA/VBE einen **linearen Framebuffer** ein und übergibt ihn in der
BootInfo. Das ist die Grundlage für alles Grafische.

## Die neue Crate-Aufteilung

| Crate            | Läuft wo            | Zweck                                    |
|------------------|---------------------|------------------------------------------|
| `bootloader_api` | im Kernel (no_std)  | entry_point!-Makro, BootInfo, Config     |
| `bootloader`     | auf dem Host (std)  | baut das bootfähige Disk-Image (MBR/GPT) |

`bootimage` und unser eigenes Target-JSON **entfallen komplett**:
- Der Kernel baut für das eingebaute Rust-Target `x86_64-unknown-none`
  (rustup liefert vorkompilierte core/alloc → kein build-std, kein
  json-target-spec, kein rustc-abi-Gefrickel mehr).
- Das Disk-Image baut ein eigenes kleines Host-Programm (`boot/`-Crate)
  mit `bootloader::BiosBoot::new(kernel).create_disk_image(pfad)`.

## Verifizierte API (0.11.15)

```rust
// Kernel-Seite:
use bootloader_api::{entry_point, BootInfo, BootloaderConfig, config::Mapping};

pub static CONFIG: BootloaderConfig = {
    let mut c = BootloaderConfig::new_default();
    c.mappings.physical_memory = Some(Mapping::Dynamic); // Komplett-Mapping
    c.frame_buffer.minimum_framebuffer_width  = Some(1280);
    c.frame_buffer.minimum_framebuffer_height = Some(720);
    c
};
entry_point!(kernel_main, config = &CONFIG);
fn kernel_main(boot_info: &'static mut BootInfo) -> ! { ... }

// BootInfo (die für uns relevanten Felder):
//   memory_regions: MemoryRegions          — Deref zu [MemoryRegion { start, end, kind }]
//   framebuffer: Optional<FrameBuffer>     — .take() -> Option<FrameBuffer>
//   physical_memory_offset: Optional<u64>  — .into_option()
// FrameBuffer: .info() -> { width, height, stride, bytes_per_pixel,
//                           pixel_format (Rgb|Bgr|U8|...) }, .buffer_mut() -> &mut [u8]
```

Host-Seite (`bootloader`-Crate): baut beim ersten Kompilieren die
BIOS-Stages selbst via `cargo install bootloader-x86_64-bios-*`
(braucht Nightly + rust-src → haben wir über rust-toolchain.toml).

## Umbauplan (in dieser Reihenfolge)

1. **Build-System:**
   - `Cargo.toml`: `bootloader` → `bootloader_api = "0.11"`;
     `[package.metadata.bootimage]` löschen; `[workspace] exclude=["boot"]`.
   - `.cargo/config.toml`: Target `x86_64-unknown-none` (eingebaut!),
     `[unstable]`-Sektion komplett löschen, Runner zeigt auf das neue
     boot-Crate. `x86_64-speedos.json` löschen.
   - `rust-toolchain.toml`: `targets = ["x86_64-unknown-none"]` ergänzen.

2. **boot/-Crate (der neue Runner, ersetzt bootimage):**
   Host-Programm, das cargo als Runner aufruft (bekommt den Kernel-ELF-
   Pfad als Argument): baut per BiosBoot das MBR-Image und startet QEMU.
   - Test-Erkennung wie bei bootimage: Binary liegt in `.../deps/` →
     Test-Modus (isa-debug-exit, -display none, -no-reboot, Timeout 300 s,
     Exit-Code 33 = Erfolg → 0, alles andere → 1).
   - Normal-Modus: QEMU-Fenster + `-serial stdio`.
   - QEMU-Suche: PATH, Fallback "C:\Program Files\qemu".
   - Runner-Aufruf mit explizitem Host-Target, damit das Root-Config-
     Target (x86_64-unknown-none) das Host-Tool nicht vergiftet.

3. **Kernel-Anpassungen:**
   - `main.rs`/`lib.rs` (Test-Entry) und alle 5 Integrationstests auf
     `bootloader_api::entry_point!(…, config = …)` umstellen;
     gemeinsame Config als `speed_os::BOOTLOADER_CONFIG` (physical
     memory Dynamic), main.rs zusätzlich mit Framebuffer-Minimum.
   - Borrow-Reihenfolge in jedem Entry: erst `framebuffer.take()`,
     dann `&'static mut BootInfo` → `&'static BootInfo` abwerten,
     dann offset kopieren + `&memory_regions` (sonst Borrow-Konflikt).
   - `memory.rs`: `MemoryMap/MemoryRegionType` → `MemoryRegions/`
     `MemoryRegionKind::Usable` (Struktur fast identisch: start/end/kind).

4. **Ausgabe-Umbau (VGA ist tot):**
   - 0xb8000 existiert nicht mehr (Grafikmodus, nicht gemappt!).
     `vga_buffer.rs` wird ersatzlos gelöscht, inkl. seiner 3 Tests und
     der CP437-Übersetzung (seriell kann UTF-8 nativ).
   - Neues Übergangs-Modul `konsole.rs`: behält die API (Color,
     set_color, clear_screen, cursor_aktivieren), implementiert sie
     aber als **ANSI-Escape-Codes auf der seriellen Leitung** — damit
     bleibt die Shell samt Farben voll benutzbar, nur eben im Terminal.
   - `print!`/`println!` (lib.rs) schreiben bis zum Framebuffer-
     Text-Renderer NUR noch seriell.
   - Übergangszustand dokumentieren: Tippen im QEMU-Fenster (PS/2),
     Ausgabe im Terminal (seriell).

5. **Framebuffer-Beweis (dieser Prompt):** Framebuffer beim Boot mit
   SpeedOS-Blau füllen, Auflösung/Format/Stride seriell loggen.
   Text-Rendering kommt im nächsten Prompt.

6. **Tests:** paging.rs verliert die VGA-Identity-Tests (VGA-Frame ist
   bedeutungslos geworden) → ersetzt durch map_page_zu auf einen
   allozierten Frame. basic_boot: VGA-Test → serieller Test.

## Risiken & Abbruchkriterien

- **Host-Build der BIOS-Stages auf Windows schlägt fehl** → STOPP,
  Optionen: UEFI-Boot mit OVMF-Firmware oder bootloader-Version pinnen.
- **QEMU liefert kein 1280x720** → Minimum weglassen und nehmen, was
  kommt (loggen!); der Code behandelt jede Auflösung generisch.
- **Kein Framebuffer in BootInfo** → serieller Log + weiterlaufen
  (Shell funktioniert über seriell auch ohne Framebuffer).
- **PS/2-Tastatur unter 0.11 tot** → wäre nur für die Demo schlimm,
  nicht für die Tests; dann separat debuggen.

## Erfolgs-Kriterien

1. `cargo run` startet QEMU: Fenster zeigt einfarbigen Framebuffer,
   Terminal zeigt Banner + Shell, Tippen im Fenster erscheint im Terminal.
2. `cargo test`: alle Suiten grün (headless, seriell, Exit-Codes).
3. Serielle Ausgabe funktioniert ab dem ersten Kernel-Befehl.
