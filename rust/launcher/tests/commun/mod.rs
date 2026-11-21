//! Aides des bancs d'essai : dossier temporaire, launcher démarré sur un port libre, client HTTP brut.
#![allow(dead_code)]

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use launcher::andy::Andy;
use launcher::claude::Claude;
use launcher::compte::Gardien;
use launcher::config::Chemins;
use launcher::gestionnaire::Gestionnaire;
use launcher::serveur::{servir, App, Visites};
use serde_json::{json, Value};
use web_tools_commun::serveur::{Requete, Serveur};

pub fn dossier(nom: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("lanceur-essai-{}-{}", nom, std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

pub fn port_libre() -> u16 {
    TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

pub fn attendre(mut condition: impl FnMut() -> bool, secondes: u64) -> bool {
    let fin = Instant::now() + Duration::from_secs(secondes);
    while Instant::now() < fin {
        if condition() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    false
}

pub fn chemins(d: &Path) -> Chemins {
    Chemins {
        racine: d.to_path_buf(),
        catalogue: d.join("outils.json"),
        local: d.join("config.local.json"),
        etat: d.join("etat.json"),
        journaux: d.join("journaux"),
    }
}

pub fn exe() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_launcher"))
}

pub fn gestionnaire(d: &Path, conf: Value) -> Arc<Gestionnaire> {
    let mut conf = conf.as_object().cloned().unwrap();
    conf.entry("dossier_outils").or_insert(json!(d.to_string_lossy()));
    Gestionnaire::new(conf, chemins(d), exe(), Box::new(|_: &str| {}))
}

/// Un launcher complet sur le dossier `d` (comptes, Andy, Temp...), sans réseau.
pub fn app(d: &Path, conf: Value) -> Arc<App> {
    let mut gardien = Gardien::new(&d.join("c.json"), &d.join("s.json"));
    gardien.pause_echec = Duration::ZERO;
    let mut claude = Claude::new(&d.join("web_launcher"));
    claude.liste = d.join("claude.list");
    let mut infos = serde_json::Map::new();
    infos.insert("port".into(), json!(0));
    infos.insert("dossier_outils".into(), json!(d.to_string_lossy()));
    Arc::new(App {
        g: gestionnaire(d, conf),
        gardien: Arc::new(gardien),
        andy: Andy::new(d),
        claude,
        alias: None,
        infos,
        visites: Visites::new("launcher"),
        web: Path::new(env!("CARGO_MANIFEST_DIR")).join("../../web"),
        relancer: Arc::new(|| {}),
    })
}

/// Le serveur principal du launcher, en fond ; rend son port.
pub fn demarrer(app: Arc<App>) -> (u16, Arc<Serveur>) {
    let srv = Serveur::ouvrir(0, false, 1).unwrap();
    srv.servir_en_fond(Arc::new(move |r: &mut Requete| servir(r, &app)));
    (srv.port(), srv)
}

pub struct Rep {
    pub code: u16,
    pub entetes: Vec<(String, String)>,
    pub corps: Vec<u8>,
}

impl Rep {
    pub fn entete(&self, nom: &str) -> Option<&str> {
        self.entetes.iter().find(|(k, _)| k.eq_ignore_ascii_case(nom)).map(|(_, v)| v.as_str())
    }
    pub fn tous(&self, nom: &str) -> Vec<String> {
        self.entetes.iter().filter(|(k, _)| k.eq_ignore_ascii_case(nom)).map(|(_, v)| v.clone()).collect()
    }
    pub fn json(&self) -> Value {
        serde_json::from_slice(&self.corps).unwrap_or(Value::Null)
    }
    pub fn texte(&self) -> String {
        String::from_utf8_lossy(&self.corps).into_owned()
    }
}

pub fn lire_reponse(r: &[u8]) -> Rep {
    let fin = r.windows(4).position(|w| w == b"\r\n\r\n").unwrap_or(r.len());
    let tete = String::from_utf8_lossy(&r[..fin]).into_owned();
    let mut lignes = tete.split("\r\n");
    let code = lignes.next().unwrap_or("").split(' ').nth(1).and_then(|c| c.parse().ok()).unwrap_or(0);
    let entetes = lignes.filter_map(|l| l.split_once(':')).map(|(k, v)| (k.trim().to_string(), v.trim().to_string())).collect();
    Rep { code, entetes, corps: r.get(fin + 4..).unwrap_or(&[]).to_vec() }
}

/// Une requête HTTP/1.0 ; Host « 127.0.0.1:port » si les en-têtes n'en donnent pas.
pub fn demander(port: u16, methode: &str, chemin: &str, entetes: &[(&str, &str)], corps: &[u8]) -> Rep {
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
    let mut t = format!("{} {} HTTP/1.0\r\n", methode, chemin);
    if !entetes.iter().any(|(k, _)| k.eq_ignore_ascii_case("Host")) {
        t += &format!("Host: 127.0.0.1:{}\r\n", port);
    }
    for (k, v) in entetes {
        t += &format!("{}: {}\r\n", k, v);
    }
    if methode == "POST" || !corps.is_empty() {
        t += &format!("Content-Length: {}\r\n", corps.len());
    }
    t += "\r\n";
    s.write_all(t.as_bytes()).unwrap();
    let _ = s.write_all(corps);
    let mut r = Vec::new();
    let _ = s.read_to_end(&mut r);
    lire_reponse(&r)
}

pub fn get(port: u16, chemin: &str, entetes: &[(&str, &str)]) -> Rep {
    demander(port, "GET", chemin, entetes, b"")
}

/// POST avec l'en-tête X-Launcher.
pub fn post(port: u16, chemin: &str, corps: &str, cookie: Option<&str>) -> Rep {
    let mut e = vec![("X-Launcher", "1")];
    if let Some(c) = cookie {
        e.push(("Cookie", c));
    }
    demander(port, "POST", chemin, &e, corps.as_bytes())
}
