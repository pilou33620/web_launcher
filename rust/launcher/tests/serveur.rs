//! Serveur principal du launcher (ce poste, 127.0.0.1) : connexion, Temp, Sécurité, Andy, chemins.
mod commun;

use commun::*;
use serde_json::json;
use std::os::unix::ffi::OsStrExt;

fn jeton(rep: &Rep) -> String {
    rep.entete("Set-Cookie").unwrap().split(';').next().unwrap().to_string()
}

#[test]
fn serveur_protege() {
    let d = dossier("protege");
    let app = app(&d, json!({"outils": []}));
    assert_eq!(app.gardien.ajouter("pilou", "correct horse battery"), None);
    let (port, _srv) = demarrer(app.clone());
    assert_eq!(get(port, "/", &[]).code, 302);
    assert_eq!(get(port, "/console.html", &[]).code, 302);
    assert_eq!(get(port, "/api/etat", &[]).code, 401);
    assert_eq!(get(port, "/api/journal/x", &[]).code, 401);
    assert_eq!(post(port, "/api/verifier_tout", "", None).code, 401);
    assert_eq!(get(port, "/connexion.html", &[]).code, 200);
    assert_eq!(post(port, "/api/connexion", r#"{"login":"pilou","mot_de_passe":"non"}"#, None).code, 401);
    // sans l'en-tête maison : refusé, même avec le bon mot de passe
    let sans = demander(port, "POST", "/api/connexion", &[], br#"{"login":"pilou","mot_de_passe":"correct horse battery"}"#);
    assert_eq!(sans.code, 403);
    assert_eq!(post(port, "/api/connexion", "pas du json", None).code, 400);
    assert_eq!(post(port, "/api/connexion", &"x".repeat(9000), None).code, 413);
    let r = post(port, "/api/connexion", r#"{"login":"pilou","mot_de_passe":"correct horse battery"}"#, None);
    assert_eq!(r.code, 200);
    let biscuit = r.entete("Set-Cookie").unwrap().to_string();
    assert!(biscuit.contains("HttpOnly") && biscuit.contains("SameSite=Strict") && !biscuit.contains("Secure"), "{biscuit}");
    let c = jeton(&r);
    let etat = get(port, "/api/etat", &[("Cookie", c.as_str())]);
    assert_eq!(etat.code, 200);
    let v = etat.json();
    assert_eq!(v["launcher"]["utilisateur"], "pilou");
    assert_eq!(v["launcher"]["compte"], true);
    let cles: Vec<&str> = v["launcher"].as_object().unwrap().keys().map(String::as_str).collect();
    for k in ["port", "dossier_outils", "ip", "ici", "adresses", "comptes", "masques", "mdns", "version", "maj", "action", "heure", "heures_maj", "prochaine_verif"] {
        assert!(cles.contains(&k), "{k} absent de /api/etat");
    }
    assert_eq!(get(port, "/", &[("Cookie", c.as_str())]).code, 200);
    assert_eq!(get(port, "/connexion.html", &[("Cookie", c.as_str())]).code, 302);
    assert_eq!(demander(port, "PUT", "/api/x", &[("Cookie", c.as_str())], b"").code, 405);
    assert_eq!(get(port, "/api/inconnue", &[("Cookie", c.as_str())]).json(), json!({"erreur": "route inconnue"}));
    post(port, "/api/deconnexion", "", Some(&c));
    assert_eq!(get(port, "/api/etat", &[("Cookie", c.as_str())]).code, 401);
    // nom d'hôte inconnu (DNS rebinding) : refusé
    assert_eq!(get(port, "/connexion.html", &[("Host", "piege.example")]).code, 403);
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn sans_compte_ce_poste_seulement() {
    let d = dossier("sanscompte");
    let (port, _srv) = demarrer(app(&d, json!({"outils": []})));
    assert_eq!(get(port, "/api/etat", &[]).code, 200);
    // ... sauf par Caddy / Funnel : la requête vient aussi de 127.0.0.1, mais d'Internet
    for e in launcher::serveur::ENTETES_PROXY {
        assert_eq!(get(port, "/api/etat", &[(e, "1.2.3.4")]).code, 401, "{e}");
    }
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn chemins_et_origines() {
    let d = dossier("chemins");
    let app = app(&d, json!({"outils": []}));
    let (port, _srv) = demarrer(app);
    // le launcher ne relaie jamais les outils
    assert_eq!(get(port, "/o/faux/page", &[("Accept", "text/html")]).code, 404);
    assert_eq!(get(port, "/page", &[("Referer", "http://127.0.0.1/o/faux/")]).code, 404);
    // « /css/../ » ne contourne pas la connexion
    for c in ["/css/../js/app.js", "/css/%2e%2e/js/app.js", "/css/..%2findex.html", "/icones/%2e%2e/index.html", "/css//../index.html"] {
        assert_eq!(get(port, c, &[("X-Forwarded-For", "1.2.3.4")]).code, 404, "{c}");
    }
    let css = get(port, "/css/style.css", &[("X-Forwarded-For", "1.2.3.4")]);
    assert_eq!(css.code, 200);
    assert!(css.entete("Content-Type").unwrap().starts_with("text/css"));
    assert_eq!(css.entete("X-Content-Type-Options"), Some("nosniff"));
    assert_eq!(css.entete("Content-Security-Policy"), Some("frame-ancestors 'none'"));
    assert_eq!(get(port, "/icones/manifest.json", &[("X-Forwarded-For", "1.2.3.4")]).code, 200);
    assert_eq!(get(port, "/js/andy_donnees.json", &[]).code, 404); // plus dans web/ (dépôt public)
    assert_eq!(get(port, "/.git/config", &[]).code, 404);
    assert_eq!(get(port, "/README.md", &[]).code, 404);
    let _ = std::fs::remove_dir_all(&d);
}

fn preparer_temp(nom: &str) -> (std::path::PathBuf, u16, std::sync::Arc<web_tools_commun::serveur::Serveur>) {
    let d = dossier(nom);
    let t = d.join("temp");
    std::fs::create_dir_all(t.join("sous")).unwrap();
    std::fs::create_dir_all(t.join(".cache")).unwrap();
    let fichiers: [(&str, &[u8]); 9] = [
        ("a.png", b"\x89PNG\0"),
        ("notes.txt", "été".as_bytes()),
        ("sous/rapport.md", b"# r"),
        ("page.html", b"<script>"),
        ("image.SVG", b"<svg/>"),
        ("doc.pdf", b"%PDF"),
        ("bin é.dat", b"a\0b"),
        (".cache/x.txt", b"x"),
        (".cache.txt", b"x"),
    ];
    for (n, c) in fichiers {
        std::fs::write(t.join(n), c).unwrap();
    }
    std::fs::write(d.join("secret.txt"), "secret").unwrap();
    std::os::unix::fs::symlink(d.join("secret.txt"), t.join("lien.txt")).unwrap();
    std::os::unix::fs::symlink(&d, t.join("lien_dossier")).unwrap();
    nix::unistd::mkfifo(&t.join("tube"), nix::sys::stat::Mode::from_bits_truncate(0o600)).unwrap();
    let mut mauvais = t.as_os_str().as_bytes().to_vec();
    mauvais.extend_from_slice(b"/mauvais\xff.txt"); // nom illisible en UTF-8 : ni listé ni servi
    std::fs::write(std::ffi::OsStr::from_bytes(&mauvais), "x").unwrap();
    let (port, srv) = demarrer(app(&d, json!({"outils": []})));
    (d, port, srv)
}

fn fichier(port: u16, chemin: &str, suite: &str) -> Rep {
    let enc: String = chemin.bytes().map(|b| if b.is_ascii_alphanumeric() { (b as char).to_string() } else { format!("%{:02X}", b) }).collect();
    get(port, &format!("/api/temp/fichier?chemin={}{}", enc, suite), &[])
}

#[test]
fn temp_liste() {
    let (d, port, _srv) = preparer_temp("templiste");
    let r = get(port, "/api/temp", &[]);
    assert_eq!(r.code, 200);
    let v = r.json();
    assert_eq!(v["dossier"], d.join("temp").to_string_lossy().as_ref());
    let mut genres: Vec<(String, String)> =
        v["fichiers"].as_array().unwrap().iter().map(|f| (f["chemin"].as_str().unwrap().to_string(), f["genre"].as_str().unwrap().to_string())).collect();
    genres.sort();
    let attendu: Vec<(String, String)> = [
        ("a.png", "image"),
        ("bin é.dat", "autre"),
        ("doc.pdf", "pdf"),
        ("image.SVG", "image"),
        ("notes.txt", "texte"),
        ("page.html", "html"),
        ("sous/rapport.md", "texte"),
    ]
    .iter()
    .map(|(a, b)| (a.to_string(), b.to_string()))
    .collect();
    assert_eq!(genres, attendu);
    let futur = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(9_999_999_999);
    std::fs::File::options().write(true).open(d.join("temp/doc.pdf")).unwrap().set_modified(futur).unwrap();
    assert_eq!(launcher::serveur::lister_temp(&d.join("temp"))[0]["chemin"], "doc.pdf"); // le plus récent d'abord
    assert!(launcher::serveur::lister_temp(&d.join("absent")).is_empty());
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn temp_refus_et_contenu() {
    let (d, port, _srv) = preparer_temp("temprefus");
    let secret = d.join("secret.txt").to_string_lossy().into_owned();
    for c in ["", "../secret.txt", "sous/../../secret.txt", secret.as_str(), "lien.txt", "lien_dossier/secret.txt", "tube", "sous", ".cache.txt",
              ".cache/x.txt", "absent.txt", "a\0b", "mauvais\u{fffd}.txt", "sous//rapport.md"] {
        assert_eq!(fichier(port, c, "").code, 404, "{c:?}");
    }
    for (c, type_, cadre, csp) in [("a.png", "image/png", false, false), ("notes.txt", "text/plain", false, false), ("page.html", "text/html", true, true),
                                   ("image.SVG", "image/svg+xml", false, true), ("doc.pdf", "application/pdf", true, false)] {
        let r = fichier(port, c, "");
        assert_eq!(r.code, 200, "{c}");
        assert!(r.entete("Content-Type").unwrap().starts_with(type_), "{c}");
        assert_eq!(r.entete("X-Content-Type-Options"), Some("nosniff"));
        let csps = r.tous("Content-Security-Policy");
        assert_eq!(csps.iter().any(|v| v.contains("frame-ancestors 'none'")), !cadre, "{c}");
        assert_eq!(csps.iter().any(|v| v.starts_with("sandbox;")), csp, "{c}");
        assert!(r.entete("Content-Disposition").is_none(), "{c}");
        assert!(r.entete("X-Apercu-Cadre").is_none());
    }
    let r = fichier(port, "bin é.dat", "");
    assert_eq!((r.code, r.corps.as_slice()), (200, &b"a\0b"[..]));
    assert_eq!(r.entete("Content-Type"), Some("application/octet-stream"));
    assert_eq!(r.entete("Content-Disposition"), Some("attachment; filename*=UTF-8''bin%20%C3%A9.dat"));
    assert_eq!(fichier(port, "sous/rapport.md", "").corps, b"# r");
    assert_eq!(fichier(port, "notes.txt", "&apercu=1").corps, "été".as_bytes());
    let gros = launcher::serveur::APERCU_TEMP as usize + 10;
    std::fs::write(d.join("temp/gros.log"), vec![b'x'; gros]).unwrap();
    let apercu = fichier(port, "gros.log", "&apercu=1").corps;
    assert!(apercu.len() < gros + 100 && String::from_utf8_lossy(&apercu).contains("suite non affich"));
    assert_eq!(fichier(port, "gros.log", "").corps.len(), gros);
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn dossier_securite() {
    let d = dossier("securite");
    let s = d.join("rpi_securite");
    std::fs::create_dir_all(s.join(".git")).unwrap();
    for n in ["diagramme.html", "script.sh", ".git/config"] {
        std::fs::write(s.join(n), "x").unwrap();
    }
    std::fs::write(d.join("secret.txt"), "secret").unwrap();
    std::os::unix::fs::symlink(d.join("secret.txt"), s.join("lien.txt")).unwrap();
    let (port, _srv) = demarrer(app(&d, json!({"outils": []})));
    let v = get(port, "/api/securite", &[]).json();
    let mut noms: Vec<&str> = v["fichiers"].as_array().unwrap().iter().map(|f| f["chemin"].as_str().unwrap()).collect();
    noms.sort();
    assert_eq!(noms, vec!["diagramme.html", "script.sh"]);
    let r = get(port, "/api/securite/fichier?chemin=diagramme.html", &[]);
    assert_eq!(r.code, 200);
    assert!(r.tous("Content-Security-Policy").iter().any(|c| c.contains("sandbox")));
    for c in [".git/config", "lien.txt", "../secret.txt"] {
        assert_eq!(get(port, &format!("/api/securite/fichier?chemin={}", c), &[]).code, 404, "{c}");
    }
    assert_eq!(get(port, "/api/autre/fichier?chemin=diagramme.html", &[]).code, 404);
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn andy_api() {
    let d = dossier("andy");
    let (port, _srv) = demarrer(app(&d, json!({"outils": []})));
    let r = get(port, "/api/andy/pesees", &[]);
    assert_eq!((r.code, r.json()), (200, json!({"pesees": []})));
    assert_eq!(demander(port, "POST", "/api/andy/pesees", &[], br#"{"date": "2026-01-01", "poids": 5}"#).code, 403);
    assert_eq!(post(port, "/api/andy/pesees", "pas du json", None).code, 400);
    assert_eq!(post(port, "/api/andy/pesees", "[1, 2]", None).code, 400);
    assert_eq!(post(port, "/api/andy/pesees", r#"{"date": "2026-01-01", "poids": 0}"#, None).code, 400);
    let r = post(port, "/api/andy/pesees", r#"{"date": "2026-01-01", "poids": 5.5}"#, None);
    assert_eq!(r.code, 200);
    let pesee = r.json()["pesee"].clone();
    assert_eq!(get(port, "/api/andy/pesees", &[]).json()["pesees"], json!([pesee]));
    assert_eq!(post(port, "/api/andy/pesees/supprimer", r#"{"id": "inconnu"}"#, None).code, 404);
    let corps = format!(r#"{{"id": {}}}"#, pesee["id"]);
    assert_eq!(post(port, "/api/andy/pesees/supprimer", &corps, None).code, 200);
    assert_eq!(get(port, "/api/andy/pesees", &[]).json()["pesees"], json!([]));
    assert_eq!(post(port, "/api/andy/pesees/autre", "{}", None).code, 404);
    let r = post(port, "/api/andy/evenements", r#"{"date": "2026-01-01", "type": "soin", "titre": "Détartrage"}"#, None);
    assert_eq!(r.code, 200);
    assert_eq!(post(port, "/api/andy/evenements", r#"{"date": "2026-01-01", "type": "x", "titre": "a"}"#, None).code, 400);
    assert_eq!(get(port, "/api/andy/evenements", &[]).json()["evenements"].as_array().unwrap().len(), 1);
    let corps = format!(r#"{{"id": {}}}"#, r.json()["evenement"]["id"]);
    assert_eq!(post(port, "/api/andy/evenements/supprimer", &corps, None).code, 200);
    assert_eq!(get(port, "/andy.html", &[]).code, 200);
    // historique absent du Pi (et pas de git dans ce dossier) : 404
    assert_eq!(get(port, "/api/andy/donnees", &[]).code, 404);
    std::fs::write(d.join("andy_donnees.local.json"), r#"[{"date": "2026-01-01", "type": "vaccin", "titre": "x"}]"#).unwrap();
    assert_eq!(get(port, "/api/andy/donnees", &[]).json(), json!([{"date": "2026-01-01", "type": "vaccin", "titre": "x"}]));
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn comptes_depuis_la_page() {
    let d = dossier("comptes");
    let app = app(&d, json!({"outils": []}));
    let (port, _srv) = demarrer(app.clone());
    // sans compte, ce poste crée le premier
    assert_eq!(post(port, "/api/comptes/ajouter", r#"{"login":"pilou","mot_de_passe":"court"}"#, None).code, 400);
    assert_eq!(post(port, "/api/comptes/ajouter", r#"{"login":"pilou","mot_de_passe":"un mot de passe long"}"#, None).code, 200);
    let c = jeton(&post(port, "/api/connexion", r#"{"login":"pilou","mot_de_passe":"un mot de passe long"}"#, None));
    assert_eq!(post(port, "/api/comptes/ajouter", r#"{"login":"marie","mot_de_passe":"un autre mot de passe"}"#, Some(&c)).code, 200);
    assert_eq!(get(port, "/api/etat", &[("Cookie", c.as_str())]).json()["launcher"]["comptes"].as_array().unwrap().len(), 2);
    assert_eq!(post(port, "/api/comptes/supprimer", r#"{"login":"marie"}"#, Some(&c)).code, 200);
    assert_eq!(post(port, "/api/comptes/supprimer", r#"{"login":"pilou"}"#, Some(&c)).code, 400);
    assert_eq!(post(port, "/api/comptes/autre", "{}", Some(&c)).code, 404);
    assert_eq!(post(port, "/api/outils/absent/demarrer", "", Some(&c)).json(), json!({"erreur": "outil ou action inconnu"}));
    assert_eq!(post(port, "/api/console/absent", r#"{"texte":"x"}"#, Some(&c)).code, 404);
    assert_eq!(post(port, "/api/console/absent", r#"{"texte":"\u001b"}"#, Some(&c)).code, 400);
    let _ = std::fs::remove_dir_all(&d);
}
