// speedhttp::tests — die URL-Aufloesung, auf dem HOST
//
// ===========================================================================
// WARUM AUSGERECHNET HIER TESTS STEHEN
//
// Diese Kiste hatte bis Serie 8 keine eigenen Tests, und das war richtig:
// Ihr Inhalt war der UNVERAENDERTE Parser aus Serie 5, und seine Tests
// stehen in `src/netz/http.rs` — dass sie dort unveraendert durchlaufen,
// ist der Beweis, dass der Umzug nichts angefasst hat. Diese Datei aendert
// daran nichts; sie prueft NUR die Aufloesung, die in Teil 8 dazukam.
//
// Und die braucht Tests dringender als alles andere in der Kiste: Eine
// falsch aufgeloeste relative URL fuehrt nicht zu einem Absturz, sondern
// zu einem 404 — und dann sucht man den Fehler im Netz-Stack.

use super::*;

fn basis(text: &str) -> Ziel {
    ziel_parsen(text).expect("Basis-URL")
}

/// Kurzschreibweise: loest auf und liefert den Text (ohne Fragment).
fn auf(basis_text: &str, referenz: &str) -> String {
    verweis_aufloesen(&basis(basis_text), referenz)
        .expect("aufloesbar")
        .ziel
        .als_text()
}

// ===========================================================================
// Die Grundfaelle
// ===========================================================================

#[test]
fn test_absolute_referenz_gewinnt() {
    assert_eq!(
        auf("https://a.example/x/y.html", "https://b.example/z"),
        "https://b.example/z"
    );
    // Auch das Schema darf wechseln — in beide Richtungen.
    assert_eq!(
        auf("https://a.example/x/", "http://a.example/y"),
        "http://a.example/y"
    );
    assert_eq!(
        auf("http://a.example/x/", "https://a.example/y"),
        "https://a.example/y"
    );
}

#[test]
fn test_absoluter_pfad() {
    assert_eq!(
        auf("https://a.example/x/y.html", "/neu/seite.html"),
        "https://a.example/neu/seite.html"
    );
}

#[test]
fn test_relativ_gegen_das_verzeichnis() {
    // Der Dateiname der Basis faellt weg.
    assert_eq!(
        auf("https://a.example/x/y.html", "z.html"),
        "https://a.example/x/z.html"
    );
    // Endet die Basis auf `/`, IST sie das Verzeichnis.
    assert_eq!(
        auf("https://a.example/x/", "z.html"),
        "https://a.example/x/z.html"
    );
    // Wurzel ohne Pfad.
    assert_eq!(auf("https://a.example", "z.html"), "https://a.example/z.html");
}

#[test]
fn test_unterverzeichnis() {
    assert_eq!(
        auf("https://a.example/x/y.html", "bilder/logo.png"),
        "https://a.example/x/bilder/logo.png"
    );
}

// ===========================================================================
// Punkt-Segmente — der Teil, den `naechste_url` nicht kann
// ===========================================================================

#[test]
fn test_doppelpunkt_geht_nach_oben() {
    assert_eq!(
        auf("https://a.example/x/y/z.html", "../oben.html"),
        "https://a.example/x/oben.html"
    );
    assert_eq!(
        auf("https://a.example/x/y/z.html", "../../ganz-oben.html"),
        "https://a.example/ganz-oben.html"
    );
}

#[test]
fn test_einzelner_punkt_faellt_weg() {
    assert_eq!(
        auf("https://a.example/x/y.html", "./z.html"),
        "https://a.example/x/z.html"
    );
    assert_eq!(
        auf("https://a.example/x/y.html", "./a/./b/c.html"),
        "https://a.example/x/a/b/c.html"
    );
}

/// **`..` ueber die Wurzel hinaus verpufft.**
///
/// Das ist nicht nur die Spezifikation (RFC 3986 §5.2.4), sondern die
/// Sicherheitsfrage dieser Funktion: Ohne sie koennte ein `href` aus dem
/// Dokumentbaum eines Servers herauszeigen — und bei LOKALEN Dateien,
/// die derselbe Normalisierer bedient, aus dem Seiten-Ordner heraus.
#[test]
fn test_ueber_die_wurzel_hinaus_verpufft() {
    assert_eq!(
        auf("https://a.example/x/y.html", "../../../../../etc/passwd"),
        "https://a.example/etc/passwd"
    );
    assert_eq!(pfad_normalisieren("/../../x"), "/x");
    assert_eq!(pfad_normalisieren("/a/../../x"), "/x");
}

#[test]
fn test_schluss_schraegstrich_bleibt() {
    // `/a/b/` und `/a/b` sind fuer viele Server VERSCHIEDENE Seiten.
    assert_eq!(pfad_normalisieren("/a/b/"), "/a/b/");
    assert_eq!(pfad_normalisieren("/a/b"), "/a/b");
    assert_eq!(
        auf("https://a.example/x/y.html", "unter/"),
        "https://a.example/x/unter/"
    );
    // Auch `..` mit Schluss-Schraegstrich.
    assert_eq!(
        auf("https://a.example/x/y/z.html", "../"),
        "https://a.example/x/"
    );
}

#[test]
fn test_normalisieren_einzeln() {
    assert_eq!(pfad_normalisieren("/a/b/../c"), "/a/c");
    assert_eq!(pfad_normalisieren("/a/./b"), "/a/b");
    assert_eq!(pfad_normalisieren("/a//b"), "/a/b");
    assert_eq!(pfad_normalisieren("/"), "/");
    assert_eq!(pfad_normalisieren("/a/b/.."), "/a");
}

// ===========================================================================
// Fragmente
// ===========================================================================

#[test]
fn test_fragment_wird_abgetrennt() {
    let v = verweis_aufloesen(&basis("https://a.example/x/y.html"), "z.html#kapitel").unwrap();
    assert_eq!(v.ziel.als_text(), "https://a.example/x/z.html");
    assert_eq!(v.fragment.as_deref(), Some("kapitel"));
    assert!(!v.gleiche_seite, "andere Datei");
}

/// **Ein `href="#oben"` laedt GAR NICHTS.** Der haeufigste Verweis auf
/// einer Wikipedia-Seite — wer ihn als Ladevorgang behandelt, holt bei
/// jedem Klick aufs Inhaltsverzeichnis die ganze Seite neu.
#[test]
fn test_nur_fragment_ist_dieselbe_seite() {
    let v = verweis_aufloesen(&basis("https://a.example/x/y.html"), "#oben").unwrap();
    assert_eq!(v.ziel.als_text(), "https://a.example/x/y.html");
    assert_eq!(v.fragment.as_deref(), Some("oben"));
    assert!(v.gleiche_seite, "nur das Fragment aendert sich");
}

#[test]
fn test_leerer_verweis_ist_dieselbe_seite() {
    let v = verweis_aufloesen(&basis("https://a.example/x/y.html"), "").unwrap();
    assert!(v.gleiche_seite);
    assert_eq!(v.fragment, None);
}

/// Ein Verweis auf dieselbe Datei, aber ausgeschrieben — auch das ist
/// dieselbe Seite.
#[test]
fn test_gleiche_seite_wird_erkannt() {
    let v = verweis_aufloesen(&basis("https://a.example/x/y.html"), "y.html#tief").unwrap();
    assert!(v.gleiche_seite, "derselbe Ort, nur anderes Fragment");
}

#[test]
fn test_fragment_kommt_nie_in_die_anfrage() {
    let v = verweis_aufloesen(&basis("https://a.example/"), "/suche?q=1#treffer").unwrap();
    assert_eq!(v.ziel.url.pfad, "/suche?q=1", "kein # im Pfad");
    assert!(!v.ziel.anfrage().contains('#'));
}

// ===========================================================================
// Query
// ===========================================================================

#[test]
fn test_query_referenz_behaelt_den_pfad() {
    assert_eq!(
        auf("https://a.example/suche?q=alt", "?q=neu"),
        "https://a.example/suche?q=neu"
    );
}

#[test]
fn test_relativ_ersetzt_die_query_der_basis() {
    // Die Query der BASIS gehoert nicht zum Verzeichnis.
    assert_eq!(
        auf("https://a.example/x/y.html?a=1", "z.html"),
        "https://a.example/x/z.html"
    );
}

#[test]
fn test_punkte_in_der_query_bleiben() {
    // In einer Query ist `..` ein gewoehnlicher Text und darf nicht
    // wegnormalisiert werden.
    assert_eq!(
        auf("https://a.example/", "/x?pfad=../geheim"),
        "https://a.example/x?pfad=../geheim"
    );
}

// ===========================================================================
// Schema-relativ und fremde Schemata
// ===========================================================================

#[test]
fn test_schema_relativ_uebernimmt_das_schema() {
    assert_eq!(
        auf("https://a.example/x/", "//cdn.example/bild.png"),
        "https://cdn.example/bild.png"
    );
    assert_eq!(
        auf("http://a.example/x/", "//cdn.example/bild.png"),
        "http://cdn.example/bild.png"
    );
}

/// `mailto:` und `javascript:` sind KEINE kaputten URLs — wir koennen sie
/// nur nicht besuchen. Der Unterschied steht im Fehlerwert, damit ein
/// Browser das Richtige sagen kann.
#[test]
fn test_fremde_schemata_sind_nicht_navigierbar() {
    for referenz in [
        "mailto:wer@example.com",
        "javascript:void(0)",
        "tel:+49123",
        "ftp://a.example/datei",
        "data:text/html,<b>x</b>",
    ] {
        assert_eq!(
            verweis_aufloesen(&basis("https://a.example/"), referenz),
            Err(HttpFehler::SchemaNichtNavigierbar),
            "{} sollte als nicht navigierbar gemeldet werden",
            referenz
        );
    }
}

/// Ein Doppelpunkt IM PFAD macht daraus kein Schema.
#[test]
fn test_doppelpunkt_im_pfad_ist_kein_schema() {
    assert_eq!(
        auf("https://a.example/x/", "bilder/a:b.png"),
        "https://a.example/x/bilder/a:b.png"
    );
    // Und einer hinter einem `?` auch nicht.
    assert_eq!(
        auf("https://a.example/x/", "?zeit=12:30"),
        "https://a.example/x/?zeit=12:30"
    );
}

// ===========================================================================
// Ports und Sonderfaelle
// ===========================================================================

#[test]
fn test_port_bleibt_bei_relativen_verweisen() {
    assert_eq!(
        auf("http://a.example:8080/x/y.html", "z.html"),
        "http://a.example:8080/x/z.html"
    );
}

#[test]
fn test_leerraum_wird_geschnitten() {
    assert_eq!(
        auf("https://a.example/x/", "  z.html  "),
        "https://a.example/x/z.html"
    );
}

/// PANICKT NIE — auch bei Muell nicht. Ein `href` kommt von einer fremden
/// Seite; er ist Eingabe wie jede andere.
#[test]
fn test_muell_panickt_nicht() {
    let b = basis("https://a.example/x/y.html");
    for referenz in [
        "://", "//", "///", "?", "#", "..", "../..", "/", ":", "a:", "%%%", "\u{0}",
        "sehr/tief/../../../../../..", "\t\n", "http://", "https://",
    ] {
        // Ergebnis egal — es darf nur nicht panicken.
        let _ = verweis_aufloesen(&b, referenz);
    }
}

/// Die Aufloesung ist STABIL: Ein schon aufgeloester Verweis, noch einmal
/// aufgeloest, ergibt dasselbe. Ohne diese Eigenschaft koennte der
/// Schleifenschutz des Verlaufs nicht auf Textvergleichen beruhen.
#[test]
fn test_aufloesung_ist_stabil() {
    let b = basis("https://a.example/x/y.html");
    let einmal = verweis_aufloesen(&b, "../z/../w.html").unwrap().ziel;
    let zweimal = verweis_aufloesen(&einmal, &einmal.als_text()).unwrap().ziel;
    assert_eq!(einmal.als_text(), zweimal.als_text());
    assert_eq!(einmal.als_text(), "https://a.example/w.html");
}

// ===========================================================================
// Die Zusage an den Rest des Projekts
// ===========================================================================

/// Die Serie-5- und Serie-7-Funktionen sind unangetastet: Sie liefern
/// weiter GENAU dasselbe wie vorher, auch dort, wo die neue Aufloesung
/// etwas anderes tut.
///
/// Der interessante Fall ist `..`: `naechste_url` normalisiert NICHT (es
/// muss nicht — eine `Location:` mit `..` ist selten und Server verstehen
/// sie). Wer beide verwechselt, bekommt zwei verschiedene Texte fuer
/// denselben Ort, und der Schleifenschutz zaehlt falsch.
#[test]
fn test_alte_funktionen_unveraendert() {
    let alt = url_parsen("http://a.example/x/y.html").unwrap();
    assert_eq!(
        naechste_url(&alt, "../z.html").unwrap().pfad,
        "/x/../z.html",
        "naechste_url normalisiert bewusst NICHT — das ist der Serie-5-Stand"
    );
    // Die neue Aufloesung tut es.
    assert_eq!(
        auf("http://a.example/x/y.html", "../z.html"),
        "http://a.example/z.html"
    );
    // Und `url_parsen` lehnt https weiter ab.
    assert_eq!(
        url_parsen("https://a.example"),
        Err(HttpFehler::TlsNichtUnterstuetzt)
    );
}
