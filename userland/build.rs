// userland/build.rs — reicht das Linker-Skript an jedes Programm durch.
//
// Der Pfad wird hier zur Bauzeit ABSOLUT gemacht (aus CARGO_MANIFEST_DIR).
// Das ist bewusst so: Ein relativer Pfad in .cargo/config.toml würde vom
// Arbeitsverzeichnis des Aufrufers abhängen — und der Kernel-build.rs ruft
// uns aus einem anderen Verzeichnis als ein Mensch, der `cargo build` in
// userland/ tippt. Absolut funktioniert beides.

fn main() {
    let verzeichnis = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR fehlt");
    let skript = format!("{verzeichnis}/speedos.ld");

    // --no-pie: ERZEUGT ET_EXEC STATT ET_DYN. Zwei Dinge haengen daran:
    //
    //  (1) `src/elf.rs` laedt nur ET_EXEC — ein ET_DYN muesste beim Laden
    //      relokiert werden, und einen Relokations-Verarbeiter hat SpeedOS
    //      bewusst nicht.
    //  (2) Viel subtiler: Ein PIE-Link zieht die Sektionen des dynamischen
    //      Linkens mit (.dynsym, .gnu.hash, .hash, .dynstr, .rela.dyn,
    //      .dynamic). Die stehen in KEINEM Skript-Abschnitt, lld legt sie
    //      deshalb als "Waisen" direkt hinter .text — MITTEN in unsere
    //      sorgfaeltig ausgerichtete Segment-Folge. Ergebnis waren zwei
    //      PT_LOADs mit verschiedenen Rechten in derselben Seite, und
    //      `elf::pruefen` lehnt das (zu Recht) als W^X-Luecke ab.
    //      Ohne PIE existieren diese Sektionen gar nicht erst.
    println!("cargo:rustc-link-arg-bins=--no-pie");
    // -T<skript>: rust-lld nimmt unser Skript statt seiner Voreinstellung.
    // Nur fuer BINARIES (-bins) — die Bibliothek selbst wird nie gelinkt.
    println!("cargo:rustc-link-arg-bins=-T{skript}");
    // Ohne das richtet lld Segmente an 2 MiB aus und blaeht die Datei auf
    // Megabytes auf. Wir wollen 4-KiB-Seiten, so wie sie der Loader mappt.
    println!("cargo:rustc-link-arg-bins=-zmax-page-size=4096");

    println!("cargo:rerun-if-changed={skript}");
    println!("cargo:rerun-if-changed=build.rs");
}
