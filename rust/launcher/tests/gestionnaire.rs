//! Cycle de vie avec un faux web tool (dépôt git local, aucun accès réseau) : installer, démarrer,
//! mettre à jour, reprendre, relancer, terminal, passage en Rust, réglages, apps ajoutées.
mod commun;

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use commun::*;
use launcher::config;
use launcher::gestionnaire::{Erreur, Gestionnaire, Outil};
use launcher::reseau::port_ouvert;
use serde_json::{json, Value};

const SERVEUR: &str = r#"import http.server, sys
VERSION = %VERSION%
class H(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        self.send_response(200); self.end_headers(); self.wfile.write(VERSION.encode())
    def log_message(self, *a): pass
print("pret", flush=True)
http.server.HTTPServer(("127.0.0.1", int(sys.argv[1])), H).serve_forever()
"#;

fn serveur(version: &str) -> String {
    SERVEUR.replace("%VERSION%", &format!("{:?}", version))
}

fn git(dossier: &Path, args: &[&str]) {
    let o = Command::new("git").args(["-c", "user.name=t", "-c", "user.email=t@t"]).args(args).current_dir(dossier).output().unwrap();
    assert!(o.status.success(), "git {:?} : {}", args, String::from_utf8_lossy(&o.stderr));
}

struct Faux {
    tmp: PathBuf,
    travail: PathBuf,
    documents: PathBuf,
    port: u16,
    g: Arc<Gestionnaire>,
    o: Arc<Outil>,
    lignes: Arc<Mutex<Vec<String>>>,
}

impl Drop for Faux {
    fn drop(&mut self) {
        if self.o.pid().is_some() {
            self.g.arreter(&self.o);
        }
        let _ = std::fs::remove_dir_all(&self.tmp);
    }
}

fn conf_faux(tmp: &Path, documents: &Path, port: u16) -> Value {
    json!({"dossier_outils": documents.to_string_lossy(), "verification_maj_heures": 0, "outils": [{
        "id": "faux", "nom": "Faux", "depot": tmp.join("origine.git").to_string_lossy(), "dossiers": ["faux", "FAUX_ALT"],
        "port": port, "commande": ["{python}", "serveur.py", "{port}"], "delai_arret": 5}]})
}

fn gestionnaire_faux(tmp: &Path, conf: &Value, lignes: &Arc<Mutex<Vec<String>>>) -> Arc<Gestionnaire> {
    let l = lignes.clone();
    Gestionnaire::new(conf.as_object().cloned().unwrap(), chemins(tmp), exe(), Box::new(move |t: &str| l.lock().unwrap().push(t.to_string())))
}

fn faux(nom: &str) -> Faux {
    let tmp = dossier(nom);
    let (origine, travail) = (tmp.join("origine.git"), tmp.join("travail"));
    assert!(Command::new("git").args(["init", "-q", "--bare", "-b", "main"]).arg(&origine).status().unwrap().success());
    assert!(Command::new("git").args(["clone", "-q"]).arg(&origine).arg(&travail).output().unwrap().status.success());
    let documents = tmp.join("Documents");
    std::fs::create_dir_all(&documents).unwrap();
    let port = port_libre();
    let lignes = Arc::new(Mutex::new(Vec::new()));
    let g = gestionnaire_faux(&tmp, &conf_faux(&tmp, &documents, port), &lignes);
    let o = g.outil("faux").unwrap();
    let f = Faux { tmp, travail, documents, port, g, o, lignes };
    f.publier("v1");
    f
}

impl Faux {
    fn publier(&self, version: &str) {
        std::fs::write(self.travail.join("serveur.py"), serveur(version)).unwrap();
        git(&self.travail, &["add", "-A"]);
        git(&self.travail, &["commit", "-q", "-m", &format!("version {}", version)]);
        git(&self.travail, &["push", "-q", "origin", "HEAD:main"]);
    }

    fn action(&self, nom: &str) {
        self.g.lancer_action("faux", nom).unwrap();
        assert!(attendre(|| self.o.e().action.is_none(), 60), "action {} trop longue", nom);
        for _ in 0..3 {
            self.g.actualiser(&self.o);
        }
    }

    fn page(&self) -> String {
        let mut s = TcpStream::connect(("127.0.0.1", self.port)).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        s.write_all(b"GET / HTTP/1.0\r\n\r\n").unwrap();
        let mut r = String::new();
        let _ = s.read_to_string(&mut r);
        r.split("\r\n\r\n").nth(1).unwrap_or("").to_string()
    }

    fn en_marche(&self) -> bool {
        attendre(|| port_ouvert(self.port), 30)
    }

    fn etat_voulu(&self) -> Value {
        launcher::lire_json(&self.tmp.join("etat.json")).unwrap()["outils"]["faux"].clone()
    }
}

#[test]
fn cycle_complet() {
    let f = faux("cycle");
    assert!(!f.o.installe());
    f.action("installer");
    assert!(f.o.installe(), "{}", f.o.e().message);
    assert_eq!(f.o.version()["sujet"], "version v1");
    f.action("demarrer");
    assert!(f.en_marche());
    f.g.actualiser(&f.o);
    assert_eq!(f.o.e().phase, "en_marche");
    assert_eq!(f.page(), "v1");
    assert_eq!(f.etat_voulu()["voulu"], true);
    // une nouvelle version sur « GitHub » : vue, puis appliquée pendant que l'outil tourne
    f.publier("v2");
    f.action("verifier");
    assert_eq!(f.o.e().maj.nombre, 1);
    assert_eq!(f.o.e().maj.nouveautes, vec!["version v2".to_string()]);
    let ancien = f.o.pid();
    f.action("mettre_a_jour");
    assert_ne!(f.o.pid(), ancien);
    assert!(f.en_marche());
    assert!(attendre(|| f.page() == "v2", 20));
    assert_eq!(f.o.e().maj.nombre, 0);
    // une modification locale bloque la mise à jour, sans rien écraser
    f.publier("v3");
    let mut fichier = std::fs::OpenOptions::new().append(true).open(f.o.dossier().join("serveur.py")).unwrap();
    writeln!(fichier, "# modifié sur le Pi").unwrap();
    f.action("mettre_a_jour");
    assert!(f.o.e().message.contains("modifiés"), "{}", f.o.e().message);
    assert_eq!(f.page(), "v2");
    f.action("arreter");
    assert!(f.o.pid().is_none());
    assert!(!port_ouvert(f.port));
    assert_eq!(f.etat_voulu()["voulu"], false);
}

#[test]
fn passage_en_rust() {
    // un outil réécrit en Rust (serveur.py remplacé par lancer.sh) : relancé sans toucher au catalogue
    passage_en_rust_avec("rust", "binaires.sh", "lancer.sh");
}

#[test]
fn passage_en_rust_scripts() {
    // méthode web_suite : demarrer_<NOM>.sh, exécutables pris par scripts/binaires.sh
    passage_en_rust_avec("rust-scripts", "scripts/binaires.sh", "demarrer_FAUX.sh");
}

fn passage_en_rust_avec(nom: &str, binaires: &str, lanceur: &str) {
    let f = faux(nom);
    f.action("installer");
    assert!(!f.o.rust());
    std::fs::remove_file(f.travail.join("serveur.py")).unwrap();
    std::fs::write(f.travail.join("moteur.py"), serveur("rust")).unwrap(); // l'« exécutable » Rust
    std::fs::create_dir_all(f.travail.join("scripts")).unwrap();
    std::fs::write(f.travail.join(binaires), "obtenir_binaires() { mkdir -p bin && echo pris >> bin/compte; }\n").unwrap();
    std::fs::write(f.travail.join(lanceur), format!(". ./{}\nobtenir_binaires && exec python3 moteur.py \"$@\"\n", binaires)).unwrap();
    std::fs::write(f.travail.join(".gitignore"), "bin/\n").unwrap();
    git(&f.travail, &["add", "-A"]);
    git(&f.travail, &["commit", "-q", "-m", "portage en Rust"]);
    git(&f.travail, &["push", "-q", "origin", "HEAD:main"]);
    f.action("mettre_a_jour");
    assert!(f.o.installe(), "{}", f.o.e().message);
    assert!(f.o.rust());
    assert_eq!(f.o.commande(), vec!["sh", lanceur, "{port}"]);
    assert_eq!(std::fs::read_to_string(f.o.dossier().join("bin/compte")).unwrap(), "pris\n"); // exécutables pris pendant la mise à jour
    f.action("demarrer");
    assert!(f.en_marche());
    assert!(attendre(|| f.page() == "rust", 20));
}

#[test]
fn demarreur_rust() {
    // outil porté en Rust : sh demarrer_<NOM>.sh s'il existe, sinon sh lancer.sh, avec les mêmes options
    let f = faux("demarreur");
    let d = f.o.dossier();
    std::fs::create_dir_all(&d).unwrap();
    let creer = |nom: &str| std::fs::write(d.join(nom), "").unwrap();
    assert_eq!(f.o.commande(), vec!["{python}", "serveur.py", "{port}"]); // ni script ni lanceur
    creer("lancer.sh");
    assert_eq!(f.o.commande(), vec!["sh", "lancer.sh", "{port}"]);
    creer("demarrer_FAUX.sh");
    assert_eq!(f.o.commande(), vec!["sh", "demarrer_FAUX.sh", "{port}"]);
    assert!(f.o.installe());
    creer("serveur.py");
    assert_eq!(f.o.commande(), vec!["{python}", "serveur.py", "{port}"]); // resté en Python
    f.o.e().conf.insert("commande".into(), json!(["sh", "demarrer_FAUX.sh", "--sans-maj", "{port}"]));
    assert!(!f.o.rust());
    assert_eq!(f.o.commande(), vec!["sh", "demarrer_FAUX.sh", "--sans-maj", "{port}"]); // déjà au catalogue : tel quel
}

#[test]
fn relais_des_connexions() {
    let f = faux("connexions");
    std::fs::create_dir_all(f.tmp.join("journaux")).unwrap();
    let j = f.o.journal();
    std::fs::write(&j, "  [connexion] avant le launcher\n").unwrap();
    f.g.relayer_connexions(&f.o); // l'ancien contenu n'est pas relayé
    let ajouter = |t: &str| std::fs::OpenOptions::new().append(true).open(&j).unwrap().write_all(t.as_bytes()).unwrap();
    ajouter("bavardage\r\n  [connexion] 10:00:00  Faux  192.168.1.20  ->  1 appareil\r\n  [connexion] coup");
    f.g.relayer_connexions(&f.o);
    assert_eq!(*f.lignes.lock().unwrap(), vec!["  [connexion] 10:00:00  Faux  192.168.1.20  ->  1 appareil".to_string()]);
    ajouter("ée\n");
    f.g.relayer_connexions(&f.o); // la ligne coupée arrive entière
    assert_eq!(f.lignes.lock().unwrap().last().unwrap(), "  [connexion] coupée");
    std::fs::write(&j, "  [connexion] neuf\n").unwrap(); // journal renouvelé au démarrage
    f.g.relayer_connexions(&f.o);
    assert_eq!(f.lignes.lock().unwrap().last().unwrap(), "  [connexion] neuf");
}

#[test]
fn mise_a_jour_automatique() {
    let f = faux("majauto");
    f.action("installer");
    f.action("demarrer");
    assert!(f.en_marche());
    f.publier("v2");
    assert!(f.o.e().maj_auto); // activée par défaut
    f.g.regler_maj_auto("faux", false).unwrap();
    f.g.verifier_tous(); // sans « MAJ auto » : vu, pas installé
    assert_eq!(f.o.e().maj.nombre, 1);
    assert_eq!(f.page(), "v1");
    f.g.regler_maj_auto("faux", true).unwrap();
    assert_eq!(f.etat_voulu()["maj_auto"], true);
    f.g.verifier_tous();
    assert!(f.en_marche());
    assert!(attendre(|| f.page() == "v2", 20));
    assert_eq!(f.o.e().maj.nombre, 0);
    assert!(f.o.e().message.contains("automatiquement"), "{}", f.o.e().message);
}

#[test]
fn reprise_et_relance() {
    let f = faux("reprise");
    f.action("installer");
    f.action("demarrer");
    assert!(f.en_marche());
    let pid = f.o.pid().unwrap();
    // un nouveau launcher (après mise à jour ou plantage) retrouve l'outil sans le relancer
    let g2 = gestionnaire_faux(&f.tmp, &conf_faux(&f.tmp, &f.documents, f.port), &f.lignes);
    g2.reprendre();
    assert_eq!(g2.outil("faux").unwrap().pid(), Some(pid));
    // plantage : relance automatique, l'outil était voulu allumé
    nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), nix::sys::signal::Signal::SIGKILL).unwrap();
    assert!(attendre(
        || {
            f.g.actualiser(&f.o);
            !matches!(f.o.pid(), None) && f.o.pid() != Some(pid)
        },
        20
    ));
    assert!(f.en_marche());
    g2.outil("faux").unwrap().e().pid = None; // g2 n'est plus utilisé
}

#[test]
fn dossier_alternatif_et_occupe() {
    let f = faux("alternatif");
    assert!(Command::new("git").args(["clone", "-q"]).arg(f.tmp.join("origine.git")).arg(f.documents.join("faux_alt")).output().unwrap().status.success());
    assert!(f.o.installe());
    assert!(f.o.dossier().ends_with("faux_alt"));
    assert!(f.o.prendre());
    f.o.e().action = Some("Test".into());
    assert!(matches!(f.g.lancer_action("faux", "demarrer"), Err(Erreur::Occupe(m)) if m == "Test en cours"));
    assert!(matches!(f.g.lancer_action("faux", "inconnue"), Err(Erreur::Inconnu)));
    assert!(matches!(f.g.lancer_action("absent", "demarrer"), Err(Erreur::Inconnu)));
    f.o.e().action = None;
    f.o.rendre();
}

#[test]
fn terminal_question_et_reponse() {
    // l'outil tourne dans un terminal : on lit sa question et on lui répond depuis la page
    let f = faux("terminal");
    std::fs::write(
        f.travail.join("serveur.py"),
        format!("import sys\nnom = input('Votre nom ? ')\nprint('Bonjour', nom, sys.stdin.isatty())\n{}", serveur("v1")),
    )
    .unwrap();
    git(&f.travail, &["commit", "-q", "-am", "question"]);
    git(&f.travail, &["push", "-q", "origin", "HEAD:main"]);
    f.action("installer");
    f.action("demarrer");
    assert!(
        attendre(
            || {
                f.g.actualiser(&f.o);
                f.o.e().attend_saisie
            },
            15
        ),
        "la question n'est pas vue : {}",
        std::fs::read_to_string(f.o.journal()).unwrap_or_default()
    );
    assert!(!port_ouvert(f.port));
    f.g.envoyer("faux", "Pi\r").unwrap();
    assert!(f.en_marche());
    let (position, texte) = launcher::serveur::lire_depuis(&f.o.journal(), 0);
    assert!(texte.contains("Bonjour Pi True"), "{texte}");
    assert_eq!(position, std::fs::metadata(f.o.journal()).unwrap().len());
    // Ctrl+C : l'outil s'arrête normalement, il n'est pas relancé
    f.g.envoyer("faux", "\x03").unwrap();
    assert!(attendre(
        || {
            f.g.actualiser(&f.o);
            f.o.pid().is_none()
        },
        20
    ));
    assert_eq!(f.o.e().phase, "arrete");
    assert!(!f.o.e().voulu);
    assert!(matches!(f.g.envoyer("faux", "x"), Err(Erreur::Valeur(_))));
}

#[test]
fn reglages_depuis_la_page() {
    let tmp = dossier("reglages");
    std::fs::write(tmp.join("config.local.json"), r#"{"verification_maj_heures": 12}"#).unwrap();
    let (p1, p2) = (port_libre(), port_libre());
    let g = gestionnaire(
        &tmp,
        json!({"port": 80, "port_secours": 8090, "ordre": ["a", "b"],
               "outils": [{"id": "a", "nom": "A", "port": p1, "depot": "x"}, {"id": "b", "nom": "B", "port": p2, "depot": "x"}]}),
    );
    for port in [json!("abc"), json!(80), json!(22), json!(p2)] {
        assert!(matches!(g.regler("a", Some(&port), None), Err(Erreur::Valeur(_))), "{port}");
    }
    let nouveau = port_libre();
    g.regler("a", Some(&json!(nouveau)), None).unwrap();
    assert_eq!(g.outil("a").unwrap().port(), nouveau);
    g.regler("a", None, Some(&json!(false))).unwrap();
    assert_eq!(g.outils().iter().map(|o| o.id.clone()).collect::<Vec<_>>(), vec!["b"]);
    let local = tmp.join("config.local.json");
    assert_eq!(config::charger(&tmp.join("absent.json"), &local).unwrap()["verification_maj_heures"], 12);
    assert_eq!(launcher::lire_json(&local).unwrap()["outils"], json!([{"id": "a", "port": nouveau, "masque": true}]));
    g.regler("a", None, Some(&json!(true))).unwrap();
    assert_eq!(g.outils().iter().map(|o| o.id.clone()).collect::<Vec<_>>(), vec!["a", "b"]);
    g.outil("b").unwrap().e().pid = Some(std::process::id() as i32); // en marche : réglages refusés
    assert!(matches!(g.regler("b", Some(&json!(port_libre())), None), Err(Erreur::Occupe(_))));
    assert!(matches!(g.regler("z", Some(&json!(9000)), None), Err(Erreur::Inconnu)));
    g.outil("b").unwrap().e().pid = None;
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn ajouter_une_app() {
    let tmp = dossier("ajouter");
    std::fs::create_dir_all(tmp.join("mon_app")).unwrap();
    std::fs::write(tmp.join("mon_app/server.py"), "").unwrap();
    std::fs::create_dir_all(tmp.join("vide")).unwrap();
    let g = gestionnaire(&tmp, json!({"port": 80, "port_secours": 8090, "outils": [{"id": "a", "nom": "A", "port": 8790, "depot": "x"}]}));
    for (url, opts) in [
        ("https://github.com/autre/mon_app", ""),
        ("https://github.com/pilou33620/../x", ""),
        ("https://github.com/pilou33620/mon_app", "; rm -rf ~"),
        ("https://github.com/pilou33620/mon_app", "--port 9"),
        ("https://github.com/pilou33620/A", ""),
        ("https://github.com/pilou33620/vide", ""),
    ] {
        assert!(g.ajouter(url, opts).is_err(), "{url} {opts}");
    }
    assert_eq!(g.ajouter("https://github.com/pilou33620/mon_app.git", "--lan").unwrap(), "mon_app");
    let o = g.outil("mon_app").unwrap();
    assert!(o.installe());
    assert_eq!(o.e().conf["commande"], json!(["{python}", "server.py", "--lan", "--port", "{port}"]));
    assert!(8790 < o.port() && o.port() < 8900);
    let conf = config::charger(&tmp.join("absent.json"), &tmp.join("config.local.json")).unwrap();
    let vus: Vec<(Value, Value, Value)> =
        conf["outils"].as_array().unwrap().iter().map(|c| (c["id"].clone(), c["port"].clone(), c["commande"][1].clone())).collect();
    assert_eq!(vus, vec![(json!("mon_app"), json!(o.port()), json!("server.py"))]);
    assert!(g.outil("vide").is_none());
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn ports_des_sous_outils() {
    let tmp = dossier("sousoutils");
    // parent arrêté : les ports du catalogue livré
    let cat = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../outils.json");
    let mut conf = config::charger(&cat, &tmp.join("absent.json")).unwrap();
    for o in conf.get_mut("outils").unwrap().as_array_mut().unwrap() {
        o["port"] = json!(port_libre()); // le vrai WEB·SUITE tourne peut-être sur ce poste
    }
    conf.insert("dossier_outils".into(), json!(tmp.to_string_lossy()));
    let g = gestionnaire(&tmp, Value::Object(conf));
    let ports: Vec<Option<u16>> = ["web_cao", "web_antenna", "web_3d", "web_suite", "x"].iter().map(|i| g.port_sous_outil(i)).collect();
    assert_eq!(ports, vec![Some(8000), Some(8732), Some(8139), None, None]);
    // parent en marche : le port qu'il annonce, gardé 5 s
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port_parent = l.local_addr().unwrap().port();
    let vus = Arc::new(Mutex::new(0));
    let v = vus.clone();
    std::thread::spawn(move || {
        for mut s in l.incoming().flatten() {
            let mut t = [0u8; 2048];
            let _ = s.read(&mut t);
            *v.lock().unwrap() += 1;
            let corps = json!({"outils": [{"id": "web_cao", "port": 8001}, {"id": "web_3d", "port": null}]}).to_string();
            let _ = write!(s, "HTTP/1.0 200 OK\r\nContent-Length: {}\r\n\r\n{}", corps.len(), corps);
        }
    });
    let g = gestionnaire(
        &tmp,
        json!({"outils": [{"id": "web_suite", "port": port_parent, "dossiers": ["x"], "commande": ["x"],
                           "relais_sous_outils": {"web_cao": 8000, "web_3d": 8139}}]}),
    );
    assert_eq!(g.port_sous_outil("web_cao"), Some(8001)); // le port annoncé, pas celui du catalogue
    assert_eq!(g.port_sous_outil("web_3d"), Some(8139)); // arrêté : le catalogue, le relais dira 502
    assert_eq!(*vus.lock().unwrap(), 1); // pas une demande par requête relayée
    let _ = std::fs::remove_dir_all(&tmp);
}
