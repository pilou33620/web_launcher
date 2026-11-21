//! Le launcher vu d'un autre appareil (WEB_TOOLS_BANC_DISTANT=1 : ce poste est traité comme un
//! appareil du Wi-Fi) : connexion exigée partout, HTTPS sur le même port. Processus à part, car la
//! variable vaut pour tout le processus.
mod commun;

use std::process::Command;
use std::sync::Arc;

use commun::*;
use launcher::serveur::servir;
use serde_json::json;
use web_tools_commun::appairage;
use web_tools_commun::serveur::{Requete, Serveur};

fn distant() {
    std::env::set_var("WEB_TOOLS_BANC_DISTANT", "1");
    // CA du poste et jeton dans un dossier d'essai, jamais dans ~/.web_tools
    let d = std::env::temp_dir().join(format!("lanceur-distant-tls-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    std::env::set_var("WEB_TOOLS_JETON_FICHIER", d.join("jeton.txt"));
}

#[test]
fn connexion_exigee_partout() {
    distant();
    let d = dossier("distant");
    std::fs::create_dir_all(d.join("temp")).unwrap();
    std::fs::write(d.join("temp/notes.txt"), "x").unwrap();
    // sans compte : un autre appareil ne peut ni entrer ni créer le premier compte
    let (port, _srv) = demarrer(app(&d, json!({"outils": []})));
    assert_eq!(get(port, "/api/etat", &[]).code, 401);
    assert_eq!(post(port, "/api/comptes/ajouter", r#"{"login":"x","mot_de_passe":"123456789012"}"#, None).code, 401);
    assert_eq!(get(port, "/", &[]).code, 302);
    for c in ["/api/temp", "/api/temp/fichier?chemin=notes.txt", "/api/andy/donnees", "/api/andy/pesees"] {
        assert_eq!(get(port, c, &[]).code, 401, "{c}");
    }
    for c in ["/andy.html", "/js/andy.js", "/console.html"] {
        assert_eq!(get(port, c, &[]).code, 302, "{c}");
    }
    assert_eq!(post(port, "/api/andy/pesees", r#"{"date": "2026-01-01", "poids": 5}"#, None).code, 401);
    assert_eq!(post(port, "/api/andy/pesees/supprimer", r#"{"id": "x"}"#, None).code, 401);
    assert_eq!(post(port, "/api/outils/x/demarrer", "", None).code, 401);
    assert_eq!(get(port, "/connexion.html", &[]).code, 200);
    assert_eq!(get(port, "/js/connexion.js", &[]).code, 200);
    let _ = std::fs::remove_dir_all(&d);
}

fn curl(args: &[&str]) -> Option<String> {
    let o = Command::new("curl").args(args).output().ok()?;
    Some(String::from_utf8_lossy(&o.stdout).into_owned())
}

#[test]
fn https_sur_le_port() {
    distant();
    let d = dossier("https");
    let app = app(&d, json!({"outils": []}));
    assert_eq!(app.gardien.ajouter("pilou", "un-mot-de-passe-long"), None);
    let srv = Serveur::ouvrir(0, false, 1).unwrap();
    match appairage::contexte_tls() {
        Ok(cfg) => srv.brancher_tls(cfg),
        Err(raison) => {
            eprintln!("HTTPS non essayé : {}", raison); // pas d'openssl sur ce poste
            return;
        }
    }
    let a = app.clone();
    srv.servir_en_fond(Arc::new(move |r: &mut Requete| servir(r, &a)));
    let port = srv.port();
    // un autre appareil arrivé en HTTP : renvoyé vers https:// sur ce même port, avec le port
    let r = get(port, "/connexion.html", &[("Host", "192.168.1.31")]);
    assert_eq!((r.code, r.entete("Location")), (307, Some(format!("https://192.168.1.31:{}/connexion.html", port).as_str())));
    assert_eq!(get(port, "/x", &[("Host", "192.168.1.31:80")]).entete("Location"), Some("https://192.168.1.31:80/x"));
    assert_eq!(get(port, appairage::CA, &[]).code, 200); // la CA, sans connexion
    let base = format!("https://127.0.0.1:{}", port);
    let Some(code) = curl(&["-sk", "-o", "/dev/null", "-w", "%{http_code}", &format!("{}/connexion.html", base)]) else {
        eprintln!("curl absent : connexion chiffrée non essayée");
        return;
    };
    assert_eq!(code, "200");
    let tete = curl(&[
        "-sk", "-D", "-", "-o", "/dev/null", "-X", "POST", "-H", "X-Launcher: 1", "-H", "Content-Type: application/json",
        "--data", r#"{"login":"pilou","mot_de_passe":"un-mot-de-passe-long"}"#, &format!("{}/api/connexion", base),
    ])
    .unwrap()
    .to_lowercase();
    assert!(tete.starts_with("http/1.0 200"), "{tete}");
    let biscuit = tete.lines().find(|l| l.starts_with("set-cookie:")).unwrap_or("");
    assert!(biscuit.contains("secure"), "{biscuit}");
    assert!(!tete.contains("strict-transport-security")); // pas sur le Wi-Fi
    let _ = std::fs::remove_dir_all(&d);
}
