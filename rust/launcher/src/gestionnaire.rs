//! Cycle de vie des web tools : installer, démarrer, arrêter, vérifier et appliquer les mises à jour.
//!
//! Chaque outil tourne dans son propre groupe de processus et écrit dans journaux/<id>.log. Son PID
//! est gardé dans etat.local.json : si le launcher redémarre (mise à jour, plantage), il retrouve
//! les outils déjà lancés au lieu de les relancer. Un outil allumé au moment où le Raspberry Pi
//! s'éteint est rallumé au démarrage suivant.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, RwLock};
use std::time::{Duration, Instant};

use nix::errno::Errno;
use nix::sys::signal::{kill, killpg, Signal};
use nix::sys::wait::{waitpid, WaitPidFlag, WaitStatus};
use nix::unistd::{getpgid, Pid};
use regex::Regex;
use serde_json::{json, Map, Value};

use crate::config::{self, Chemins};
use crate::{depot, ecrire_json, heure, lire_json, maintenant, reseau, tenir, vrai};

const TAILLE_JOURNAL: u64 = 2 * 1024 * 1024; // au-delà, l'ancien journal devient <id>.log.1
const DELAI_DEMARRAGE: f64 = 90.0; // s pour que le port réponde
const RELANCES_MAX: usize = 3; // relances automatiques après un plantage...
const FENETRE_RELANCES: f64 = 600.0; // ...dans cette fenêtre (s)
const DUREE_CODE: f64 = 600.0; // un code d'appairage WEB·SUITE vit 10 min
const SCRIPTS_PROBABLES: [&str; 2] = ["serveur.py", "server.py"]; // point d'entrée d'une app ajoutée, dans cet ordre
const LANCEUR_RUST: &str = "lancer.sh"; // point d'entrée d'un web tool passé de Python à Rust...
const DEMARREUR_RUST: &str = "demarrer_"; // ...remplacé par demarrer_<NOM>.sh (REGLES-NOUVEAU-WEB-TOOL.md 5.1)
const BINAIRES_RUST: [&str; 3] = ["sh", "-c", ". ./binaires.sh && obtenir_binaires"]; // compile (cargo) ou télécharge
// ceux du launcher lui-même, rangés dans scripts/ comme ceux de web_suite
const BINAIRES_LAUNCHER: [&str; 3] = ["sh", "-c", ". ./scripts/binaires.sh && obtenir_binaires"];
const PLAGE_PORTS: std::ops::Range<u16> = 8790..8900; // ports des apps ajoutées (REGLES-NOUVEAU-WEB-TOOL.md 5.3)
const DEPOTS_PERMIS: &str = "https://github.com/pilou33620/"; // seules apps installables depuis la page

/// Inconnu : 404 ; Occupe : 409 ; Valeur : 400 (ou 409 pour le terminal).
#[derive(Debug)]
pub enum Erreur {
    Inconnu,
    Occupe(String),
    Valeur(String),
}

/// Heure de lancement d'un processus (Linux), pour un outil repris ; sinon maintenant.
fn heure_lancement(pid: i32) -> f64 {
    let lire = || -> Option<f64> {
        let stat = std::fs::read_to_string(format!("/proc/{}/stat", pid)).ok()?;
        let depart: f64 = stat.rsplit_once(')')?.1.split_whitespace().nth(19)?.parse().ok()?;
        let boot: f64 = std::fs::read_to_string("/proc/stat").ok()?.lines().find(|l| l.starts_with("btime"))?.split_whitespace().nth(1)?.parse().ok()?;
        // shortcut: 100 tops par seconde (USER_HZ de Linux sur arm64 et x86) ; sysconf si un noyau diffère
        Some(boot + depart / 100.0)
    };
    lire().unwrap_or_else(maintenant)
}

/// Secondes depuis l'allumage du Pi ; ailleurs, comme un Pi qui démarre.
fn duree_allume() -> f64 {
    std::fs::read_to_string("/proc/uptime").ok().and_then(|t| t.split_whitespace().next()?.parse().ok()).unwrap_or(0.0)
}

/// Les derniers `n` octets d'un fichier.
fn fin_du_fichier(chemin: &Path, n: u64) -> Option<Vec<u8>> {
    let mut f = File::open(chemin).ok()?;
    let taille = f.seek(SeekFrom::End(0)).ok()?;
    f.seek(SeekFrom::Start(taille.saturating_sub(n))).ok()?;
    let mut v = Vec::new();
    f.read_to_end(&mut v).ok()?;
    Some(v)
}

#[derive(Clone, Debug, Default)]
pub struct Maj {
    pub nombre: u64,
    pub nouveautes: Vec<String>,
    pub modifies: Vec<String>,
    pub verifie_le: Option<f64>,
    pub erreur: Option<String>,
}

impl Maj {
    pub fn json(&self) -> Value {
        json!({"nombre": self.nombre, "nouveautes": self.nouveautes, "modifies": self.modifies,
               "verifie_le": self.verifie_le, "erreur": self.erreur})
    }
}

/// Ce qui change pendant la vie d'un outil (tenu un instant seulement, jamais pendant une action).
pub struct Etat {
    pub conf: Map<String, Value>,
    pub port: u16,
    pub action: Option<String>, // action en cours (texte), ou None
    pub pid: Option<i32>,
    pub lance_le: f64,
    pub arret_demande: bool,
    pub voulu: bool,    // doit tourner (rallumé au démarrage du Pi)
    pub maj_auto: bool, // installer seul les mises à jour trouvées à chaque vérification
    pub phase: String,
    pub message: String,
    pub code_sortie: Option<i32>,
    pub relances: Vec<f64>,
    pub maj: Maj,
    pub code: Option<(String, f64)>, // (code d'appairage, vu le)
    pub attend_saisie: bool,         // une question attend une réponse dans le terminal
    pub ctrl_c_le: f64,              // Ctrl+C envoyé depuis le terminal de la page
    version: (f64, Value),
}

pub struct Outil {
    pub id: String,
    racine_outils: PathBuf,
    journaux: PathBuf,
    occupe: AtomicBool, // une action à la fois
    motif: OnceLock<Option<Regex>>,
    pub e: Mutex<Etat>,
}

impl Outil {
    pub fn new(conf: Map<String, Value>, racine_outils: &Path, journaux: &Path) -> Outil {
        let port = conf.get("port").and_then(entier).unwrap_or(0) as u16;
        Outil {
            id: conf.get("id").and_then(Value::as_str).unwrap_or("").to_string(),
            racine_outils: racine_outils.to_path_buf(),
            journaux: journaux.to_path_buf(),
            occupe: AtomicBool::new(false),
            motif: OnceLock::new(),
            e: Mutex::new(Etat {
                conf,
                port,
                action: None,
                pid: None,
                lance_le: 0.0,
                arret_demande: false,
                voulu: false,
                maj_auto: true,
                phase: "arrete".into(),
                message: String::new(),
                code_sortie: None,
                relances: Vec::new(),
                maj: Maj::default(),
                code: None,
                attend_saisie: false,
                ctrl_c_le: 0.0,
                version: (0.0, Value::Null),
            }),
        }
    }

    pub fn e(&self) -> MutexGuard<'_, Etat> {
        tenir(&self.e)
    }

    pub fn prendre(&self) -> bool {
        self.occupe.compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst).is_ok()
    }

    pub fn rendre(&self) {
        self.occupe.store(false, Ordering::SeqCst);
    }

    pub fn port(&self) -> u16 {
        self.e().port
    }

    pub fn pid(&self) -> Option<i32> {
        self.e().pid
    }

    fn champ(&self, cle: &str) -> Value {
        let e = self.e();
        let v = e.conf.get(cle).cloned().unwrap_or(Value::Null);
        v
    }

    pub fn nom(&self) -> String {
        self.champ("nom").as_str().unwrap_or(&self.id).to_string()
    }

    fn motif(&self) -> Option<&Regex> {
        self.motif.get_or_init(|| self.champ("code_appairage").as_str().and_then(|m| Regex::new(m).ok())).as_ref()
    }

    /// Commit installé, relu toutes les 15 s au plus (git est lent sur un Pi).
    pub fn version(&self) -> Value {
        {
            let e = self.e();
            if maintenant() - e.version.0 <= 15.0 {
                return e.version.1.clone();
            }
        }
        let v = if self.installe() { depot::version(&self.dossier()) } else { Value::Null };
        self.e().version = (maintenant(), v.clone());
        v
    }

    pub fn oublier_version(&self) {
        self.e().version = (0.0, Value::Null);
    }

    /// Premier dossier existant parmi conf["dossiers"] (casse ignorée), sinon le premier nom.
    pub fn dossier(&self) -> PathBuf {
        let mut noms: Vec<String> =
            self.champ("dossiers").as_array().map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect()).unwrap_or_default();
        if noms.is_empty() {
            noms.push(self.id.clone());
        }
        let presents: HashMap<String, String> = std::fs::read_dir(&self.racine_outils)
            .map(|d| d.flatten().filter_map(|e| e.file_name().to_str().map(|n| (n.to_lowercase(), n.to_string()))).collect())
            .unwrap_or_default();
        for nom in &noms {
            if let Some(vrai_nom) = presents.get(&nom.to_lowercase()) {
                return self.racine_outils.join(vrai_nom);
            }
        }
        self.racine_outils.join(&noms[0])
    }

    fn commande_catalogue(&self) -> Vec<String> {
        self.champ("commande").as_array().map(|a| a.iter().map(|v| v.as_str().unwrap_or("").to_string()).collect()).unwrap_or_default()
    }

    /// Web tool porté en Rust : son script Python a disparu ; rend son point d'entrée,
    /// demarrer_<NOM>.sh s'il existe, sinon lancer.sh. None : pas porté (un dossier resté en
    /// Python, pas encore mis à jour, garde sa commande Python).
    pub fn lanceur_rust(&self) -> Option<String> {
        let cmd = self.commande_catalogue();
        let d = self.dossier();
        if cmd.len() < 2 || cmd[0] != "{python}" || d.join(&cmd[1]).is_file() {
            return None;
        }
        let mut demarreurs: Vec<String> = std::fs::read_dir(&d)
            .map(|l| {
                l.flatten()
                    .filter(|e| e.path().is_file())
                    .filter_map(|e| e.file_name().into_string().ok())
                    .filter(|n| n.starts_with(DEMARREUR_RUST) && n.ends_with(".sh"))
                    .collect::<Vec<String>>()
            })
            .unwrap_or_default();
        demarreurs.sort();
        demarreurs.into_iter().next().or_else(|| d.join(LANCEUR_RUST).is_file().then(|| LANCEUR_RUST.to_string()))
    }

    pub fn rust(&self) -> bool {
        self.lanceur_rust().is_some()
    }

    /// La commande du catalogue ; pour un outil en Rust, « sh demarrer_<NOM>.sh » (ou
    /// « sh lancer.sh ») avec les mêmes options.
    pub fn commande(&self) -> Vec<String> {
        let cmd = self.commande_catalogue();
        match self.lanceur_rust() {
            Some(lanceur) => ["sh".to_string(), lanceur].into_iter().chain(cmd.into_iter().skip(2)).collect(),
            None => cmd,
        }
    }

    pub fn script(&self) -> String {
        let cmd = self.commande();
        cmd.get(1).or(cmd.first()).cloned().unwrap_or_default()
    }

    pub fn installe(&self) -> bool {
        let s = self.script();
        !s.is_empty() && self.dossier().join(s).is_file()
    }

    fn python(&self) -> String {
        let venv = self.dossier().join(".venv/bin/python");
        if venv.is_file() {
            venv.to_string_lossy().into_owned()
        } else {
            "python3".into()
        }
    }

    pub fn remplir(&self, morceaux: &[String]) -> Vec<String> {
        let (python, port, dossier) = (self.python(), self.port().to_string(), self.dossier().to_string_lossy().into_owned());
        morceaux.iter().map(|m| m.replace("{python}", &python).replace("{port}", &port).replace("{dossier}", &dossier)).collect()
    }

    pub fn journal(&self) -> PathBuf {
        self.journaux.join(format!("{}.log", self.id))
    }

    /// Entrée du terminal de l'outil (launcher --console).
    pub fn fifo(&self) -> PathBuf {
        self.journaux.join(format!("{}.entree", self.id))
    }

    pub fn avec_terminal(&self) -> bool {
        cfg!(target_os = "linux") && self.e().conf.get("terminal").map(vrai).unwrap_or(true)
    }

    /// Le dépôt de l'outil puis ses sous-dépôts installés (WEB·SUITE : WEB_CAO...).
    pub fn depots(&self) -> Vec<PathBuf> {
        let d = self.dossier();
        let mut liste = vec![d.clone()];
        for nom in self.champ("sous_depots").as_array().cloned().unwrap_or_default() {
            let s = d.join(nom.as_str().unwrap_or(""));
            if depot::est_depot(&s) {
                liste.push(s);
            }
        }
        liste
    }
}

/// int() de Python sur une valeur reçue en JSON.
pub fn entier(v: &Value) -> Option<i64> {
    match v {
        Value::Number(n) => n.as_i64().or_else(|| n.as_f64().filter(|x| x.is_finite()).map(|x| x.trunc() as i64)),
        Value::String(s) => s.trim().parse().ok(),
        Value::Bool(b) => Some(i64::from(*b)),
        _ => None,
    }
}

fn nom_de(d: &Path) -> String {
    d.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

/// Le processus tourne-t-il dans le dossier de l'outil ? (un PID peut avoir été réutilisé)
fn est_a_lui(o: &Outil, pid: i32) -> bool {
    let Ok(cwd) = std::fs::canonicalize(format!("/proc/{}/cwd", pid)) else { return false };
    let d = o.dossier();
    cwd.starts_with(std::fs::canonicalize(&d).unwrap_or(d))
}

fn signal(pid: i32, sig: Signal) {
    if pid <= 0 {
        return;
    }
    if sig == Signal::SIGKILL {
        // l'outil derrière son terminal (launcher --console) : il a sa propre session
        if let Ok(t) = std::fs::read_to_string(format!("/proc/{0}/task/{0}/children", pid)) {
            for enfant in t.split_whitespace().filter_map(|x| x.parse::<i32>().ok()) {
                let _ = killpg(Pid::from_raw(enfant), sig);
            }
        }
    }
    let p = Pid::from_raw(pid);
    if getpgid(Some(p)) == Ok(p) {
        let _ = killpg(p, sig); // notre outil : tout son groupe (ses sous-processus)
    } else {
        let _ = kill(p, sig);
    }
}

/// La dernière ligne du terminal est une question restée sans réponse depuis 2 s : l'outil attend
/// une saisie (ex. WEB·SUITE : « Se connecter maintenant ? [O/n] »).
fn question_en_attente(journal: &Path) -> bool {
    let Ok(m) = std::fs::metadata(journal) else { return false };
    if m.modified().ok().map(|t| t.elapsed().map(|d| d < Duration::from_secs(2)).unwrap_or(true)).unwrap_or(true) {
        return false;
    }
    let Some(fin) = fin_du_fichier(journal, 300) else { return false };
    let fin = String::from_utf8_lossy(&fin);
    let derniere = fin.rsplit('\n').next().unwrap_or("").trim();
    !derniere.is_empty() && (derniere.ends_with([':', '?', ']', '>']) || derniere.to_lowercase().contains("[o/n"))
}

/// shlex.split : mots séparés par des blancs, guillemets et barre oblique inverse.
fn decouper(texte: &str) -> Result<Vec<String>, String> {
    let mut mots = Vec::new();
    let mut mot: Option<String> = None;
    let mut c = texte.chars();
    while let Some(x) = c.next() {
        match x {
            ' ' | '\t' | '\n' | '\r' => {
                if let Some(m) = mot.take() {
                    mots.push(m);
                }
            }
            '\'' => {
                let m = mot.get_or_insert_with(String::new);
                loop {
                    match c.next() {
                        Some('\'') => break,
                        Some(y) => m.push(y),
                        None => return Err("No closing quotation".into()),
                    }
                }
            }
            '"' => {
                let m = mot.get_or_insert_with(String::new);
                loop {
                    match c.next() {
                        Some('"') => break,
                        Some('\\') => match c.next() {
                            Some(y) if y == '"' || y == '\\' => m.push(y),
                            Some(y) => {
                                m.push('\\');
                                m.push(y);
                            }
                            None => return Err("No closing quotation".into()),
                        },
                        Some(y) => m.push(y),
                        None => return Err("No closing quotation".into()),
                    }
                }
            }
            '\\' => match c.next() {
                Some(y) => mot.get_or_insert_with(String::new).push(y),
                None => return Err("No escaped character".into()),
            },
            y => mot.get_or_insert_with(String::new).push(y),
        }
    }
    mots.extend(mot);
    Ok(mots)
}

/// {id: port} des sous-outils d'après /api/etat du parent (WEB·SUITE).
fn demander_etat(port: u16) -> Option<HashMap<String, u16>> {
    let delai = Duration::from_secs(5);
    let mut s = TcpStream::connect_timeout(&SocketAddr::from((Ipv4Addr::LOCALHOST, port)), delai).ok()?;
    s.set_read_timeout(Some(delai)).ok()?;
    s.set_write_timeout(Some(delai)).ok()?;
    // en un seul envoi : un serveur qui lit une fois puis ferme ne coupe pas la demande en route
    let demande = format!("GET /api/etat HTTP/1.0\r\nHost: 127.0.0.1:{}\r\nAccept-Encoding: identity\r\n\r\n", port);
    s.write_all(demande.as_bytes()).ok()?;
    let mut r = Vec::new();
    s.take(4 * 1024 * 1024).read_to_end(&mut r).ok()?;
    let debut = r.windows(4).position(|w| w == b"\r\n\r\n")? + 4;
    let v: Value = serde_json::from_slice(&r[debut..]).ok()?;
    let mut ports = HashMap::new();
    for o in v.get("outils")?.as_array()? {
        if let (Some(id), Some(p)) = (o["id"].as_str(), entier(&o["port"]).filter(|p| *p != 0)) {
            ports.insert(id.to_string(), p as u16);
        }
    }
    Some(ports)
}

type Ecrire = Box<dyn Fn(&str) + Send + Sync>;

pub struct Gestionnaire {
    pub conf: Map<String, Value>,
    pub chemins: Chemins,
    /// Le launcher lui-même : il sert de terminal aux outils (--console).
    pub exe: PathBuf,
    outils: RwLock<Vec<Arc<Outil>>>,
    masques: Mutex<Vec<Map<String, Value>>>, // cachés (Réglages), réglages seulement
    ordre: Mutex<Vec<String>>,
    verrou_etat: Mutex<()>,
    pub launcher_maj: Mutex<Maj>,
    pub launcher_action: Mutex<Option<String>>,
    pub heures_maj: f64,
    pub prochaine_verif: Mutex<Option<f64>>, // heure de la prochaine vérification automatique
    lus: Mutex<HashMap<String, u64>>,        // id -> position déjà relayée du journal
    annonces: Mutex<HashMap<String, (Instant, HashMap<String, u16>)>>, // id du parent -> (quand, ports)
    pub ecrire: Ecrire,
}

impl Gestionnaire {
    pub fn new(conf: Map<String, Value>, chemins: Chemins, exe: PathBuf, ecrire: Ecrire) -> Arc<Gestionnaire> {
        let racine = PathBuf::from(conf.get("dossier_outils").and_then(Value::as_str).unwrap_or("."));
        let outils: Vec<Arc<Outil>> = conf
            .get("outils")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter_map(|o| o.as_object().cloned())
            .map(|o| Arc::new(Outil::new(o, &racine, &chemins.journaux)))
            .collect();
        let masques = conf.get("outils_masques").and_then(Value::as_array).cloned().unwrap_or_default();
        let ordre: Vec<String> = match conf.get("ordre").and_then(Value::as_array) {
            Some(a) if !a.is_empty() => a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect(),
            _ => outils.iter().map(|o| o.id.clone()).collect(),
        };
        let heures_maj = conf.get("verification_maj_heures").map(|v| if v.is_null() { Some(0.0) } else { v.as_f64() }).unwrap_or(Some(1.0)).unwrap_or(0.0);
        let _ = std::fs::create_dir_all(&chemins.journaux);
        Arc::new(Gestionnaire {
            conf,
            chemins,
            exe,
            outils: RwLock::new(outils),
            masques: Mutex::new(masques.into_iter().filter_map(|m| m.as_object().cloned()).collect()),
            ordre: Mutex::new(ordre),
            verrou_etat: Mutex::new(()),
            launcher_maj: Mutex::new(Maj::default()),
            launcher_action: Mutex::new(None),
            heures_maj,
            prochaine_verif: Mutex::new(None),
            lus: Mutex::new(HashMap::new()),
            annonces: Mutex::new(HashMap::new()),
            ecrire,
        })
    }

    pub fn outils(&self) -> Vec<Arc<Outil>> {
        self.outils.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn outil(&self, id: &str) -> Option<Arc<Outil>> {
        self.outils().into_iter().find(|o| o.id == id)
    }

    pub fn masques(&self) -> Vec<Map<String, Value>> {
        tenir(&self.masques).clone()
    }

    fn racine_outils(&self) -> PathBuf {
        PathBuf::from(self.conf.get("dossier_outils").and_then(Value::as_str).unwrap_or("."))
    }

    /// Outil lancé par un autre (WEB_CAO par WEB·SUITE) : pas de carte, mais joignable par le relais.
    /// Son parent dit sur quel port il tourne : il en prend un autre si le sien est occupé.
    pub fn port_sous_outil(&self, id: &str) -> Option<u16> {
        for o in self.outils() {
            let ports = o.champ("relais_sous_outils");
            if let Some(p) = ports.get(id) {
                return self.ports_annonces(&o).get(id).copied().or_else(|| entier(p).map(|x| x as u16));
            }
        }
        None
    }

    /// Gardé 5 s : chaque requête relayée passe ici, et cet état lance git status.
    fn ports_annonces(&self, o: &Outil) -> HashMap<String, u16> {
        if let Some((quand, ports)) = tenir(&self.annonces).get(&o.id) {
            if quand.elapsed() <= Duration::from_secs(5) {
                return ports.clone();
            }
        }
        let ports = demander_etat(o.port()).unwrap_or_default(); // parent arrêté : le port du catalogue
        tenir(&self.annonces).insert(o.id.clone(), (Instant::now(), ports.clone()));
        ports
    }

    // -- état persistant ----------------------------------------------

    pub fn sauver(&self) {
        let _v = tenir(&self.verrou_etat);
        let mut outils = Map::new();
        for o in self.outils() {
            let e = o.e();
            outils.insert(o.id.clone(), json!({"pid": e.pid, "voulu": e.voulu, "maj_auto": e.maj_auto}));
        }
        if let Err(err) = ecrire_json(&self.chemins.etat, &json!({ "outils": outils }), 1, false, false) {
            (self.ecrire)(&format!("  État des outils non enregistré : {}", err));
        }
    }

    /// etat.local.json ; illisible : mis de côté (jamais écrasé sans le dire).
    fn lire_etat(&self) -> Map<String, Value> {
        match lire_json(&self.chemins.etat) {
            Ok(v) => v.get("outils").and_then(Value::as_object).cloned().unwrap_or_default(),
            Err(err) => {
                let base = self.chemins.etat.with_extension("");
                let mut cote = PathBuf::from(format!("{}.abime.json", base.display()));
                let mut n = 2;
                while cote.exists() {
                    cote = PathBuf::from(format!("{}.abime-{}.json", base.display(), n));
                    n += 1;
                }
                let _ = std::fs::rename(&self.chemins.etat, &cote);
                (self.ecrire)(&format!("  {} illisible ({}) : mis de côté dans {}.", self.chemins.etat.display(), err, cote.display()));
                Map::new()
            }
        }
    }

    /// Au démarrage du launcher : retrouve les outils qui tournent, rallume ceux voulus.
    pub fn reprendre(&self) {
        let etat = self.lire_etat();
        for o in self.outils() {
            let e_ = etat.get(&o.id).cloned().unwrap_or(Value::Null);
            {
                let mut e = o.e();
                e.voulu = e_.get("voulu").map(vrai).unwrap_or(false);
                e.maj_auto = e_.get("maj_auto").map(vrai).unwrap_or(true);
            }
            let pid = e_.get("pid").and_then(Value::as_i64).filter(|p| *p > 0 && *p <= i32::MAX as i64).map(|p| p as i32);
            let port = o.port();
            match pid {
                Some(pid) if self.vivant(&o, pid) && est_a_lui(&o, pid) => {
                    {
                        let mut e = o.e();
                        e.pid = Some(pid);
                        e.lance_le = heure_lancement(pid);
                        e.phase = "demarrage".into();
                    }
                    (self.ecrire)(&format!("  {:<15} déjà en marche (PID {}), repris.", o.id, pid));
                }
                _ => {
                    if reseau::port_ouvert(port) {
                        self.adopter_par_port(&o);
                    }
                }
            }
            let (sans_pid, voulu) = {
                let e = o.e();
                (e.pid.is_none(), e.voulu)
            };
            if sans_pid && voulu && o.installe() && !reseau::port_ouvert(port) {
                (self.ecrire)(&format!("  {:<15} était allumé : redémarrage.", o.id));
                if let Err(exc) = self.lancer(&o) {
                    let mut e = o.e();
                    e.phase = "erreur".into();
                    e.message = exc;
                }
            }
        }
        self.sauver();
    }

    // -- processus ----------------------------------------------------

    fn vivant(&self, o: &Outil, pid: i32) -> bool {
        let p = Pid::from_raw(pid);
        // un enfant, lancé par ce processus ou avant son redémarrage (même PID)
        match waitpid(p, Some(WaitPidFlag::WNOHANG)) {
            Ok(WaitStatus::Exited(_, code)) => {
                o.e().code_sortie = Some(code);
                return false;
            }
            Ok(WaitStatus::Signaled(_, sig, _)) => {
                o.e().code_sortie = Some(-(sig as i32));
                return false;
            }
            _ => {}
        }
        match kill(p, None) {
            Err(Errno::ESRCH) => return false,
            Err(Errno::EPERM) => return true,
            _ => {}
        }
        match std::fs::read_to_string(format!("/proc/{}/stat", pid)) {
            Ok(t) => t.rsplit_once(')').map(|(_, r)| r.split_whitespace().next() != Some("Z")).unwrap_or(false),
            Err(_) => false,
        }
    }

    fn adopter_par_port(&self, o: &Outil) -> bool {
        match reseau::pid_sur_port(o.port()) {
            Some(pid) if est_a_lui(o, pid) => {
                {
                    let mut e = o.e();
                    e.pid = Some(pid);
                    e.lance_le = heure_lancement(pid);
                    e.phase = "en_marche".into();
                    e.message = "Lancé hors du launcher (service systemd ?), repris.".into();
                }
                (self.ecrire)(&format!("  {:<15} lancé hors du launcher (PID {}), repris.", o.id, pid));
                true
            }
            _ => false,
        }
    }

    /// Rotation seulement au démarrage : le terminal d'un outil en marche écrit dans ce fichier.
    fn ouvrir_journal(&self, o: &Outil, rotation: bool) -> std::io::Result<File> {
        let j = o.journal();
        if rotation && std::fs::metadata(&j).map(|m| m.len() > TAILLE_JOURNAL).unwrap_or(false) {
            let _ = std::fs::rename(&j, PathBuf::from(format!("{}.1", j.display())));
        }
        OpenOptions::new().create(true).append(true).open(&j)
    }

    fn noter(&self, o: &Outil, texte: &str) {
        if let Ok(mut f) = self.ouvrir_journal(o, false) {
            let _ = f.write_all(format!("[launcher {}] {}\n", heure("%d/%m %H:%M:%S"), texte).as_bytes()); // une ligne d'un bloc
        }
    }

    /// Commande d'installation : sortie dans le journal de l'outil.
    fn executer(&self, o: &Outil, cmd: &[String], cwd: &Path, delai: u64) -> Result<(), String> {
        self.noter(o, &format!("$ {}", cmd.join(" ")));
        let nom = nom_de(Path::new(&cmd[0]));
        let journal = self.ouvrir_journal(o, false).map_err(|e| format!("{} : {}", nom, e))?;
        let erreurs = journal.try_clone().map_err(|e| format!("{} : {}", nom, e))?;
        let mut c = Command::new(&cmd[0]);
        c.args(&cmd[1..])
            .current_dir(cwd)
            .stdin(Stdio::null())
            .stdout(journal)
            .stderr(erreurs)
            .env("PYTHONUNBUFFERED", "1")
            .env("GIT_TERMINAL_PROMPT", "0");
        let mut enfant = c.spawn().map_err(|e| format!("{} : {}", nom, e))?;
        let fin = Instant::now() + Duration::from_secs(delai);
        let statut = loop {
            match enfant.try_wait() {
                Ok(Some(s)) => break s,
                Ok(None) if Instant::now() < fin => std::thread::sleep(Duration::from_millis(200)),
                Ok(None) => {
                    let _ = enfant.kill();
                    let _ = enfant.wait();
                    return Err(format!("{} : délai de {} s dépassé", nom, delai));
                }
                Err(e) => return Err(format!("{} : {}", nom, e)),
            }
        };
        if !statut.success() {
            let code = statut.code().unwrap_or_else(|| -statut.signal().unwrap_or(0));
            return Err(format!("{} a échoué (code {}), voir le journal.", cmd.iter().take(3).cloned().collect::<Vec<_>>().join(" "), code));
        }
        Ok(())
    }

    pub fn lancer(&self, o: &Outil) -> Result<(), String> {
        if !o.installe() {
            return Err(format!("{} n'est pas installé.", o.nom()));
        }
        let port = o.port();
        if reseau::port_ouvert(port) {
            if self.adopter_par_port(o) {
                return Ok(());
            }
            return Err(format!("Le port {} est déjà pris par un autre programme.", port));
        }
        let cmd = o.remplir(&o.commande());
        if cmd.is_empty() {
            return Err(format!("{} : commande vide.", o.nom()));
        }
        let _ = self.ouvrir_journal(o, true);
        self.noter(o, &format!("démarrage : {}", cmd.join(" ")));
        let mut tout = cmd;
        if o.avec_terminal() {
            let tete = [self.exe.to_string_lossy().into_owned(), "--console".into(), o.fifo().to_string_lossy().into_owned(),
                        o.journal().to_string_lossy().into_owned(), "--".into()];
            tout = tete.into_iter().chain(tout).collect();
        }
        let journal = self.ouvrir_journal(o, false).map_err(|e| e.to_string())?;
        let erreurs = journal.try_clone().map_err(|e| e.to_string())?;
        let enfant = Command::new(&tout[0])
            .args(&tout[1..])
            .current_dir(o.dossier())
            .stdin(Stdio::null())
            .stdout(journal)
            .stderr(erreurs)
            .process_group(0)
            .env("PYTHONUNBUFFERED", "1")
            .env("PYTHONIOENCODING", "utf-8")
            .spawn()
            .map_err(|e| format!("{} : {}", tout[0], e))?;
        let pid = enfant.id() as i32; // l'enfant est récupéré par waitpid (vivant), même après un redémarrage
        {
            let mut e = o.e();
            e.pid = Some(pid);
            e.lance_le = maintenant();
            e.code_sortie = None;
            e.arret_demande = false;
            e.code = None;
            e.attend_saisie = false;
            e.phase = "demarrage".into();
            e.message = "Démarrage…".into();
            e.voulu = true;
        }
        self.sauver();
        Ok(())
    }

    pub fn arreter(&self, o: &Outil) {
        let pid = o.pid();
        let Some(pid) = pid else { return };
        {
            let mut e = o.e();
            e.arret_demande = true;
            e.phase = "arret".into();
            e.message = "Arrêt…".into();
        }
        self.noter(o, &format!("arrêt demandé (PID {})", pid));
        signal(pid, Signal::SIGTERM);
        let delai = o.champ("delai_arret").as_f64().unwrap_or(10.0).clamp(0.0, 3600.0);
        let fin = Instant::now() + Duration::from_secs_f64(delai);
        while Instant::now() < fin && self.vivant(o, pid) {
            std::thread::sleep(Duration::from_millis(250));
        }
        if self.vivant(o, pid) {
            self.noter(o, "ne s'arrête pas : arrêt forcé");
            signal(pid, Signal::SIGKILL);
            std::thread::sleep(Duration::from_millis(500));
            self.vivant(o, pid);
        }
        {
            let mut e = o.e();
            e.pid = None;
            e.phase = "arrete".into();
            e.message.clear();
        }
        self.sauver();
    }

    // -- actions (en arrière-plan, une à la fois par outil) ----------

    pub fn lancer_action(self: &Arc<Self>, oid: &str, nom: &str) -> Result<(), Erreur> {
        let o = self.outil(oid).ok_or(Erreur::Inconnu)?;
        let libelle = match nom {
            "demarrer" => "Démarrage",
            "arreter" => "Arrêt",
            "redemarrer" => "Redémarrage",
            "verifier" => "Recherche de mises à jour",
            "mettre_a_jour" => "Mise à jour",
            "installer" => "Installation",
            _ => return Err(Erreur::Inconnu),
        };
        if !o.prendre() {
            let a = o.e().action.clone();
            return Err(Erreur::Occupe(format!("{} en cours", a.unwrap_or_else(|| "Une action".into()))));
        }
        o.e().action = Some(libelle.into());
        let (g, nom) = (self.clone(), nom.to_string());
        std::thread::spawn(move || {
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match nom.as_str() {
                "demarrer" => g.a_demarrer(&o),
                "arreter" => {
                    o.e().voulu = false;
                    g.arreter(&o);
                    Ok(())
                }
                "redemarrer" => {
                    g.arreter(&o);
                    g.lancer(&o)
                }
                "verifier" => g.a_verifier(&o),
                "mettre_a_jour" => g.a_mettre_a_jour(&o),
                _ => g.a_installer(&o),
            }))
            .unwrap_or_else(|_| Err("erreur interne du launcher".into()));
            if let Err(exc) = r {
                {
                    let mut e = o.e();
                    e.message = exc.clone();
                    if e.pid.is_none() && ["demarrer", "redemarrer", "installer"].contains(&nom.as_str()) {
                        e.phase = "erreur".into();
                    }
                }
                g.noter(&o, &format!("{} : {}", libelle, exc));
            }
            o.e().action = None;
            o.rendre();
        });
        Ok(())
    }

    fn a_demarrer(&self, o: &Outil) -> Result<(), String> {
        if o.pid().is_none() {
            o.e().relances.clear();
            self.lancer(o)?;
        }
        Ok(())
    }

    fn a_verifier(&self, o: &Outil) -> Result<(), String> {
        o.e().message = "Recherche de mises à jour…".into();
        self.verifier(o)?;
        o.e().message.clear();
        Ok(())
    }

    fn verifier(&self, o: &Outil) -> Result<(), String> {
        let dossier = o.dossier();
        let (mut nombre, mut nouveautes, mut modifies) = (0, Vec::new(), Vec::new());
        for d in o.depots() {
            let r = match depot::verifier(&d) {
                Ok(r) => r,
                Err(exc) => {
                    let mut e = o.e();
                    e.maj.erreur = Some(exc.clone());
                    e.maj.verifie_le = Some(maintenant());
                    return Err(exc);
                }
            };
            let nom = if d != dossier { nom_de(&d) } else { String::new() };
            nombre += r.nombre;
            nouveautes.extend(r.nouveautes.iter().map(|n| if nom.is_empty() { n.clone() } else { format!("{} : {}", nom, n) }));
            modifies.extend(r.modifies.iter().map(|m| if nom.is_empty() { m.clone() } else { format!("{}/{}", nom, m) }));
        }
        nouveautes.truncate(15);
        o.e().maj = Maj { nombre, nouveautes, modifies, verifie_le: Some(maintenant()), erreur: None };
        Ok(())
    }

    fn a_mettre_a_jour(&self, o: &Outil) -> Result<(), String> {
        o.e().message = "Recherche de mises à jour…".into();
        self.verifier(o)?;
        let (nombre, modifies, etait_lance) = {
            let e = o.e();
            (e.maj.nombre, e.maj.modifies.clone(), e.pid.is_some())
        };
        if nombre == 0 {
            o.e().message = "Déjà à jour.".into();
            return Ok(());
        }
        if !modifies.is_empty() {
            return Err(format!(
                "Fichiers modifiés sur le Raspberry Pi, mise à jour bloquée : {}",
                modifies.iter().take(5).cloned().collect::<Vec<_>>().join(", ")
            ));
        }
        if etait_lance {
            self.arreter(o);
        }
        {
            let mut e = o.e();
            e.phase = "maj".into();
            e.message = "Téléchargement de la nouvelle version…".into();
        }
        self.noter(o, "mise à jour");
        let dossier = o.dossier();
        let res = (|| -> Result<(), String> {
            for d in o.depots() {
                let changes = depot::appliquer(&d)?;
                self.noter(o, &format!("{} : {} fichier(s) changé(s)", nom_de(&d), changes.len()));
                if changes.iter().any(|c| c == "requirements.txt") && vrai(&o.champ("venv")) && d == dossier {
                    o.e().message = "Mise à jour des bibliothèques Python…".into();
                    self.installer_venv(o)?;
                }
            }
            self.binaires_rust(o)
        })();
        {
            let mut e = o.e();
            e.phase = "arrete".into();
            e.version = (0.0, Value::Null);
            e.maj.nombre = 0;
            e.maj.nouveautes.clear();
            e.maj.verifie_le = Some(maintenant());
            e.maj.erreur = None;
        }
        if etait_lance {
            o.e().message = "Redémarrage…".into();
            self.lancer(o)?;
        }
        res?;
        if !etait_lance {
            o.e().message = "Mis à jour.".into();
        }
        Ok(())
    }

    fn installer_venv(&self, o: &Outil) -> Result<(), String> {
        let dossier = o.dossier();
        let venv = dossier.join(".venv");
        if !venv.is_dir() {
            let cmd = vec!["python3".to_string(), "-m".into(), "venv".into(), venv.to_string_lossy().into_owned()];
            if self.executer(o, &cmd, &dossier, 300).is_err() {
                self.noter(o, "venv impossible (sudo apt install python3-venv) : Python du système utilisé");
                return Ok(());
            }
        }
        if dossier.join("requirements.txt").is_file() {
            let python = venv.join("bin/python").to_string_lossy().into_owned();
            let cmd: Vec<String> = [python.as_str(), "-m", "pip", "install", "-q", "-r", "requirements.txt"].iter().map(|s| s.to_string()).collect();
            self.executer(o, &cmd, &dossier, 1800)?;
        }
        Ok(())
    }

    /// Outil en Rust : ses exécutables sont pris ici (installation, mise à jour) plutôt qu'au
    /// démarrage, où une compilation sur le Pi dépasserait DELAI_DEMARRAGE.
    fn binaires_rust(&self, o: &Outil) -> Result<(), String> {
        let dossier = o.dossier();
        if o.rust() && dossier.join("binaires.sh").is_file() {
            o.e().message = "Compilation ou téléchargement de l'exécutable (Rust)…".into();
            let cmd: Vec<String> = BINAIRES_RUST.iter().map(|s| s.to_string()).collect();
            self.executer(o, &cmd, &dossier, 3600)?;
        }
        Ok(())
    }

    fn a_installer(&self, o: &Arc<Outil>) -> Result<(), String> {
        let cible = o.dossier();
        if o.installe() {
            return Err(format!("Déjà installé dans {}.", cible.display()));
        }
        if cible.exists() {
            return Err(format!("Le dossier {} existe mais ne contient pas {}.", cible.display(), o.script()));
        }
        {
            let mut e = o.e();
            e.phase = "installation".into();
            e.message = "Téléchargement depuis GitHub…".into();
        }
        self.noter(o, &format!("installation dans {}", cible.display()));
        depot::cloner(o.champ("depot").as_str().unwrap_or(""), &cible)?;
        if vrai(&o.champ("ajoute")) {
            self.choisir_script(o)?;
        }
        if vrai(&o.champ("venv")) && !o.rust() {
            o.e().message = "Installation des bibliothèques Python…".into();
            self.installer_venv(o)?;
        }
        self.binaires_rust(o)?;
        for cmd in o.champ("apres_clonage").as_array().cloned().unwrap_or_default() {
            o.e().message = "Installation des compléments…".into();
            let morceaux: Vec<String> = cmd.as_array().cloned().unwrap_or_default().iter().map(|m| m.as_str().unwrap_or("").to_string()).collect();
            if !morceaux.is_empty() {
                self.executer(o, &o.remplir(&morceaux), &cible, 1800)?;
            }
        }
        o.oublier_version();
        {
            let mut e = o.e();
            e.phase = "arrete".into();
            e.message = "Installé.".into();
        }
        self.noter(o, "installation terminée");
        Ok(())
    }

    // -- launcher lui-même --------------------------------------------

    pub fn verifier_launcher(&self) -> Maj {
        let r = depot::verifier(&self.chemins.racine);
        let mut m = tenir(&self.launcher_maj);
        match r {
            Ok(r) => *m = Maj { nombre: r.nombre, nouveautes: r.nouveautes, modifies: r.modifies, verifie_le: Some(maintenant()), erreur: None },
            Err(exc) => {
                m.erreur = Some(exc);
                m.verifie_le = Some(maintenant());
            }
        }
        m.clone()
    }

    /// Applique la mise à jour du launcher et prend son nouvel exécutable. Rend vrai s'il faut redémarrer.
    pub fn mettre_a_jour_launcher(&self) -> Result<bool, String> {
        let changes = depot::appliquer(&self.chemins.racine)?;
        {
            let mut m = tenir(&self.launcher_maj);
            m.nombre = 0;
            m.nouveautes.clear();
            m.verifie_le = Some(maintenant());
            m.erreur = None;
        }
        self.sauver();
        if !changes.is_empty() && self.chemins.racine.join("scripts/binaires.sh").is_file() {
            let mut c = Command::new(BINAIRES_LAUNCHER[0]);
            c.args(&BINAIRES_LAUNCHER[1..]).current_dir(&self.chemins.racine);
            match crate::sortie(c, Duration::from_secs(1800)) {
                Ok(r) => (self.ecrire)(String::from_utf8_lossy(&r.stdout).trim_end()),
                Err(_) => (self.ecrire)("  Nouvel exécutable du launcher introuvable : l'ancien redémarre."),
            }
        }
        Ok(!changes.is_empty())
    }

    // -- surveillance -------------------------------------------------

    pub fn surveiller(self: &Arc<Self>) {
        let g = self.clone();
        std::thread::spawn(move || g.boucle_maj());
        loop {
            std::thread::sleep(Duration::from_secs(2));
            for o in self.outils() {
                // la surveillance ne doit jamais s'arrêter
                let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    self.actualiser(&o);
                    self.relayer_connexions(&o);
                }));
                if r.is_err() {
                    (self.ecrire)(&format!("  surveillance {} : erreur interne", o.id));
                }
            }
        }
    }

    /// Recopie dans la sortie du launcher les lignes [connexion] du journal de l'outil : qui
    /// utilise quel outil, depuis quelle IP.
    pub fn relayer_connexions(&self, o: &Outil) {
        let mut f = match File::open(o.journal()) {
            Ok(f) => f,
            Err(_) => {
                tenir(&self.lus).entry(o.id.clone()).or_insert(0); // pas encore de journal : tout sera neuf
                return;
            }
        };
        let taille = f.seek(SeekFrom::End(0)).unwrap_or(0);
        let mut lus = tenir(&self.lus);
        let mut lu = *lus.entry(o.id.clone()).or_insert(taille); // ce qui précède le démarrage est déjà vu
        if taille < lu {
            lu = 0; // journal renouvelé (.log.1)
        }
        let mut bloc = Vec::new();
        if f.seek(SeekFrom::Start(lu)).is_err() || f.read_to_end(&mut bloc).is_err() {
            return;
        }
        let fin = bloc.iter().rposition(|&b| b == b'\n').map(|i| i + 1).unwrap_or(0); // les lignes complètes seulement
        lus.insert(o.id.clone(), lu + fin as u64);
        drop(lus);
        for ligne in String::from_utf8_lossy(&bloc[..fin]).split(['\n', '\r']) {
            if ligne.contains("[connexion]") {
                (self.ecrire)(ligne.trim_end());
            }
        }
    }

    pub fn actualiser(&self, o: &Outil) {
        let (action, phase, pid) = {
            let e = o.e();
            (e.action.clone(), e.phase.clone(), e.pid)
        };
        if action.is_some() && ["arret", "maj", "installation"].contains(&phase.as_str()) {
            return;
        }
        let port = o.port();
        if let Some(pid) = pid {
            if !self.vivant(o, pid) {
                let (code, arret_demande, ctrl_c_le) = {
                    let mut e = o.e();
                    e.pid = None;
                    e.attend_saisie = false;
                    (e.code_sortie, e.arret_demande, e.ctrl_c_le)
                };
                self.sauver();
                // 0, ou juste après un Ctrl+C : arrêté volontairement, pas de relance
                if arret_demande || code == Some(0) || maintenant() - ctrl_c_le < 15.0 {
                    if !arret_demande {
                        o.e().voulu = false;
                        self.sauver();
                        self.noter(o, "s'est arrêté normalement");
                    }
                    let mut e = o.e();
                    e.phase = "arrete".into();
                    e.message.clear();
                    return;
                }
                let code = code.map(|c| c.to_string()).unwrap_or_else(|| "None".into());
                self.noter(o, &format!("s'est arrêté tout seul (code {})", code));
                let t = maintenant();
                let relancer = {
                    let mut e = o.e();
                    e.phase = "erreur".into();
                    e.message = format!("S'est arrêté (code {}), voir le journal.", code);
                    e.relances.retain(|x| t - x < FENETRE_RELANCES);
                    e.voulu && e.relances.len() < RELANCES_MAX
                };
                if relancer && o.prendre() {
                    let n = {
                        let mut e = o.e();
                        e.relances.push(t);
                        e.relances.len()
                    };
                    self.noter(o, &format!("relance automatique ({}/{})", n, RELANCES_MAX));
                    if let Err(exc) = self.lancer(o) {
                        o.e().message = exc;
                    }
                    o.rendre();
                }
                return;
            }
            if reseau::port_ouvert(port) {
                let mut e = o.e();
                if e.phase != "en_marche" {
                    e.phase = "en_marche".into();
                    if e.message == "Démarrage…" || e.message == "Redémarrage…" {
                        e.message.clear();
                    }
                }
            } else {
                let mut e = o.e();
                if e.phase == "en_marche" || maintenant() - e.lance_le > DELAI_DEMARRAGE {
                    e.phase = "demarrage".into();
                    e.message = format!("Ne répond pas sur le port {}, voir le journal.", port);
                }
            }
            self.lire_code(o);
            let attend = o.avec_terminal() && question_en_attente(&o.journal());
            o.e().attend_saisie = attend;
        } else if action.is_none() {
            if reseau::port_ouvert(port) {
                if !self.adopter_par_port(o) {
                    let mut e = o.e();
                    e.phase = "externe".into();
                    e.message = format!("Port {} occupé par un programme lancé hors du launcher.", port);
                }
            } else {
                let mut e = o.e();
                if ["en_marche", "demarrage", "externe"].contains(&e.phase.as_str()) {
                    e.phase = "arrete".into();
                    e.message.clear();
                }
            }
        }
    }

    /// Saisie au clavier dans le terminal de l'outil.
    pub fn envoyer(&self, oid: &str, texte: &str) -> Result<(), Erreur> {
        let o = self.outil(oid).ok_or(Erreur::Inconnu)?;
        if !o.avec_terminal() || o.pid().is_none() {
            return Err(Erreur::Valeur(format!("{} n'a pas de terminal ouvert.", o.nom())));
        }
        let mut f = OpenOptions::new()
            .write(true)
            .custom_flags(nix::fcntl::OFlag::O_NONBLOCK.bits())
            .open(o.fifo())
            .map_err(|_| Erreur::Valeur("Terminal indisponible (outil lancé hors du launcher ou avant son terminal).".into()))?;
        f.write_all(texte.as_bytes()).map_err(|e| Erreur::Valeur(format!("Terminal : {}", e)))?;
        let mut e = o.e();
        if texte.contains('\x03') {
            e.ctrl_c_le = maintenant();
        }
        e.attend_saisie = false;
        Ok(())
    }

    fn lire_code(&self, o: &Outil) {
        let Some(motif) = o.motif() else { return };
        let Some(fin) = fin_du_fichier(&o.journal(), 32768) else { return };
        let fin = String::from_utf8_lossy(&fin).into_owned();
        // seulement depuis le dernier démarrage de cet outil
        let depuis = &fin[fin.rfind("] démarrage : ").unwrap_or(0)..];
        let dernier = motif.captures_iter(depuis).last().and_then(|c| c.get(1).or_else(|| c.get(0)).map(|m| m.as_str().to_string()));
        if let Some(code) = dernier {
            let mut e = o.e();
            if e.code.as_ref().map(|c| c.0.as_str()) != Some(code.as_str()) {
                e.code = Some((code, maintenant()));
            }
        }
    }

    /// Port et affichage d'un outil arrêté (page Réglages), gardés dans config.local.json.
    pub fn regler(&self, oid: &str, port: Option<&Value>, affiche: Option<&Value>) -> Result<(), Erreur> {
        let o = self.outil(oid);
        let conf = match &o {
            Some(o) => o.e().conf.clone(),
            None => self.masques().into_iter().find(|c| c.get("id").and_then(Value::as_str) == Some(oid)).ok_or(Erreur::Inconnu)?,
        };
        let nom = conf.get("nom").and_then(Value::as_str).unwrap_or(oid).to_string();
        let suffixe = conf.get("suffixe").and_then(Value::as_str).unwrap_or("").to_string();
        if let Some(o) = &o {
            if !o.prendre() {
                return Err(Erreur::Occupe(format!("{} est occupé, réessayer dans un instant.", nom)));
            }
        }
        let interne = || -> Result<Vec<(&'static str, Option<Value>)>, Erreur> {
            if let Some(o) = &o {
                let e = o.e();
                if e.action.is_some() || e.pid.is_some() || e.phase == "externe" {
                    return Err(Erreur::Occupe(format!("Arrêter {}{} avant de changer ses réglages.", nom, suffixe)));
                }
            }
            let mut champs: Vec<(&'static str, Option<Value>)> = Vec::new();
            if let Some(p) = port {
                let p = entier(p).ok_or_else(|| Erreur::Valeur("Port : un nombre, par exemple 8080.".into()))?;
                if Some(p) != conf.get("port").and_then(entier) {
                    if !(1024..=65535).contains(&p) {
                        return Err(Erreur::Valeur("Port : entre 1024 et 65535.".into()));
                    }
                    if [self.conf.get("port"), self.conf.get("port_secours")].iter().any(|x| x.and_then(entier) == Some(p)) {
                        return Err(Erreur::Valeur(format!("Port {} : c'est celui du launcher.", p)));
                    }
                    let mut autres: Vec<Map<String, Value>> = self.outils().iter().map(|x| x.e().conf.clone()).collect();
                    autres.extend(self.masques());
                    if let Some(pris) = autres.iter().find(|x| x.get("id").and_then(Value::as_str) != Some(oid) && x.get("port").and_then(entier) == Some(p)) {
                        let texte = |k: &str| pris.get(k).and_then(Value::as_str).unwrap_or("").to_string();
                        return Err(Erreur::Valeur(format!("Port {} : déjà celui de {}{}.", p, texte("nom"), texte("suffixe"))));
                    }
                    if reseau::port_ouvert(p as u16) {
                        return Err(Erreur::Valeur(format!("Port {} : déjà utilisé par un autre programme.", p)));
                    }
                    champs.push(("port", Some(json!(p))));
                }
            }
            if let Some(a) = affiche {
                if vrai(a) == conf.get("masque").map(vrai).unwrap_or(false) {
                    champs.push(("masque", if vrai(a) { None } else { Some(json!(true)) }));
                }
            }
            if champs.is_empty() {
                return Ok(champs);
            }
            config::regler_outil(oid, &self.chemins.local, &champs).map_err(|e| Erreur::Valeur(e.to_string()))?;
            if let Some((_, Some(p))) = champs.iter().find(|(k, _)| *k == "port") {
                match &o {
                    Some(o) => {
                        let mut e = o.e();
                        e.conf.insert("port".into(), p.clone());
                        e.port = entier(p).unwrap_or(0) as u16;
                        e.message.clear();
                    }
                    None => {
                        for m in tenir(&self.masques).iter_mut().filter(|m| m.get("id").and_then(Value::as_str) == Some(oid)) {
                            m.insert("port".into(), p.clone());
                        }
                    }
                }
            }
            Ok(champs)
        };
        let r = interne();
        if let Some(o) = &o {
            o.rendre();
        }
        let champs = r?;
        if champs.iter().any(|(k, _)| *k == "masque") {
            let mut liste = self.outils();
            if affiche.map(vrai).unwrap_or(false) {
                let mut masques = tenir(&self.masques);
                if let Some(i) = masques.iter().position(|m| m.get("id").and_then(Value::as_str) == Some(oid)) {
                    let mut c = masques.remove(i);
                    c.remove("masque");
                    liste.push(Arc::new(Outil::new(c, &self.racine_outils(), &self.chemins.journaux)));
                }
                drop(masques);
                let ordre = tenir(&self.ordre).clone();
                liste.sort_by_key(|x| ordre.iter().position(|i| *i == x.id).unwrap_or(ordre.len()));
            } else if let Some(o) = &o {
                let mut c = o.e().conf.clone();
                c.insert("masque".into(), json!(true));
                tenir(&self.masques).push(c);
                liste.retain(|x| !Arc::ptr_eq(x, o));
            }
            *self.outils.write().unwrap_or_else(|e| e.into_inner()) = liste;
            self.sauver();
        }
        Ok(())
    }

    // -- apps ajoutées depuis la page ------------------------------

    /// Ajoute au launcher une app d'un dépôt de DEPOTS_PERMIS (gardée dans config.local.json) et
    /// lance son installation si son dossier manque. Rend l'id, ou le message d'erreur.
    pub fn ajouter(self: &Arc<Self>, url: &str, options: &str) -> Result<String, String> {
        let url = url.trim();
        let motif = Regex::new(&format!(r"^{}([A-Za-z0-9_-][A-Za-z0-9_.-]*?)(?:\.git)?/?$", regex::escape(DEPOTS_PERMIS))).expect("motif valide");
        let oid = motif.captures(url).map(|c| c[1].to_string()).ok_or_else(|| format!("Dépôt : une adresse {}<nom>.", DEPOTS_PERMIS))?;
        let options = decouper(options).map_err(|e| format!("Options illisibles : {}.", e))?;
        let permis = |x: &String| !x.is_empty() && x.chars().all(|c| c.is_ascii_alphanumeric() || "_=.,:/@+-".contains(c));
        if !options.iter().all(permis) {
            return Err("Options : lettres, chiffres et - _ = . , : / @ + seulement.".into());
        }
        if options.iter().any(|x| x == "--port") {
            return Err("Options : --port est ajouté par le launcher.".into());
        }
        let o = {
            let _v = tenir(&self.verrou_etat);
            let mut connus: Vec<Map<String, Value>> = self.outils().iter().map(|x| x.e().conf.clone()).collect();
            connus.extend(self.masques());
            if connus.iter().any(|c| c.get("id").and_then(Value::as_str).unwrap_or("").to_lowercase() == oid.to_lowercase()) {
                return Err(format!("{} est déjà dans le launcher.", oid));
            }
            let mut pris: Vec<i64> = connus.iter().filter_map(|c| c.get("port").and_then(entier)).collect();
            pris.extend([self.conf.get("port"), self.conf.get("port_secours")].iter().filter_map(|x| x.and_then(entier)));
            let port = PLAGE_PORTS
                .clone()
                .find(|p| !pris.contains(&i64::from(*p)) && !reseau::port_ouvert(*p))
                .ok_or_else(|| format!("Aucun port libre entre {} et {}.", PLAGE_PORTS.start, PLAGE_PORTS.end - 1))?;
            let mut commande = vec![json!("{python}"), json!(SCRIPTS_PROBABLES[0])];
            commande.extend(options.iter().map(|x| json!(x)));
            commande.extend([json!("--port"), json!("{port}")]);
            let conf = json!({"id": oid, "nom": oid, "suffixe": "", "description": "App ajoutée depuis le launcher",
                              "depot": format!("{}{}.git", DEPOTS_PERMIS, oid), "dossiers": [oid], "port": port,
                              "commande": commande, "ajoute": true});
            let conf = conf.as_object().cloned().unwrap_or_default();
            let o = Arc::new(Outil::new(conf.clone(), &self.racine_outils(), &self.chemins.journaux));
            if o.dossier().is_dir() {
                self.choisir_script(&o)?; // dossier déjà là (cloné à la main) : rien à télécharger
            }
            let conf = o.e().conf.clone(); // commande ajustée par choisir_script
            let champs: Vec<(&str, Option<Value>)> = conf.iter().filter(|(k, _)| k.as_str() != "id").map(|(k, v)| (k.as_str(), Some(v.clone()))).collect();
            config::regler_outil(&oid, &self.chemins.local, &champs).map_err(|e| e.to_string())?;
            self.outils.write().unwrap_or_else(|e| e.into_inner()).push(o.clone());
            tenir(&self.ordre).push(oid.clone());
            o
        };
        self.noter(&o, &format!("ajouté depuis la page ({}, port {})", o.champ("depot").as_str().unwrap_or(""), o.port()));
        self.sauver();
        if !o.installe() {
            let _ = self.lancer_action(&oid, "installer");
        }
        Ok(oid)
    }

    /// Point d'entrée de l'app : le premier de SCRIPTS_PROBABLES présent dans son dossier.
    fn choisir_script(&self, o: &Arc<Outil>) -> Result<(), String> {
        let dossier = o.dossier();
        let Some(script) = SCRIPTS_PROBABLES.iter().find(|s| dossier.join(s).is_file()) else {
            if o.rust() {
                return Ok(()); // app en Rust : lancée par demarrer_<NOM>.sh (ou lancer.sh) avec les mêmes options
            }
            return Err(format!(
                "Ni {} ni {} dans {} : ajouter l'app à la main dans config.local.json.",
                SCRIPTS_PROBABLES.join(" ni "),
                LANCEUR_RUST,
                dossier.display()
            ));
        };
        let commande = {
            let mut e = o.e();
            match e.conf.get_mut("commande").and_then(Value::as_array_mut) {
                Some(c) if c.len() > 1 && c[1] != *script => {
                    c[1] = json!(script);
                    Some(Value::Array(c.clone()))
                }
                _ => None,
            }
        };
        if let Some(commande) = commande {
            if self.outil(&o.id).is_some_and(|x| Arc::ptr_eq(&x, o)) {
                config::regler_outil(&o.id, &self.chemins.local, &[("commande", Some(commande))]).map_err(|e| e.to_string())?;
            }
        }
        Ok(())
    }

    pub fn regler_maj_auto(&self, oid: &str, valeur: bool) -> Result<(), Erreur> {
        let o = self.outil(oid).ok_or(Erreur::Inconnu)?;
        o.e().maj_auto = valeur;
        self.noter(&o, &format!("mise à jour automatique {}", if valeur { "activée" } else { "désactivée" }));
        self.sauver();
        Ok(())
    }

    /// Vérification automatique : le launcher et chaque outil installé ; les outils en « mise à
    /// jour auto » installent aussitôt ce qui est trouvé (et redémarrent).
    pub fn verifier_tous(&self) {
        self.verifier_launcher();
        for o in self.outils() {
            if !o.installe() || !o.prendre() {
                continue; // occupé : ce sera pour la prochaine fois
            }
            o.e().action = Some("Recherche de mises à jour".into());
            let r = (|| -> Result<(), String> {
                self.verifier(&o)?;
                let (auto, n, propre) = {
                    let e = o.e();
                    (e.maj_auto, e.maj.nombre, e.maj.modifies.is_empty())
                };
                if auto && n > 0 && propre {
                    o.e().action = Some("Mise à jour automatique".into());
                    self.noter(&o, &format!("mise à jour automatique : {} nouveauté(s)", n));
                    self.a_mettre_a_jour(&o)?;
                    o.e().message = format!("Mis à jour automatiquement le {}.", heure("%d/%m à %H:%M"));
                }
                Ok(())
            })();
            if let Err(exc) = r {
                {
                    let mut e = o.e();
                    if e.maj_auto {
                        e.message = format!("Mise à jour automatique : {}", exc);
                    }
                }
                self.noter(&o, &format!("vérification automatique : {}", exc));
            }
            o.e().action = None;
            o.rendre();
        }
    }

    /// Vérification à chaque démarrage du launcher, puis toutes les N heures (si réglé).
    fn boucle_maj(&self) {
        let delai = if duree_allume() < 180.0 { 30.0 } else { 5.0 }; // Pi qui démarre : laisser le réseau se connecter
        *tenir(&self.prochaine_verif) = Some(maintenant() + delai);
        std::thread::sleep(Duration::from_secs_f64(delai));
        loop {
            self.verifier_tous();
            if self.heures_maj <= 0.0 {
                *tenir(&self.prochaine_verif) = None;
                return;
            }
            *tenir(&self.prochaine_verif) = Some(maintenant() + self.heures_maj * 3600.0);
            std::thread::sleep(Duration::from_secs_f64(self.heures_maj * 3600.0));
        }
    }

    // -- vue ----------------------------------------------------------

    pub fn etat(&self) -> Vec<Value> {
        self.outils()
            .iter()
            .map(|o| {
                let (dossier, installe, version, terminal) = (o.dossier(), o.installe(), o.version(), o.avec_terminal());
                let sous: Vec<String> = if installe { o.depots().iter().skip(1).map(|d| nom_de(d)).collect() } else { Vec::new() };
                let e = o.e();
                let t = maintenant();
                let code = match (&e.code, e.pid) {
                    (Some((c, vu)), Some(_)) if t - vu < DUREE_CODE => json!({"code": c, "age": (t - vu) as i64}),
                    _ => Value::Null,
                };
                let texte = |k: &str, defaut: &str| e.conf.get(k).cloned().unwrap_or_else(|| json!(defaut));
                let depot = e.conf.get("depot").and_then(Value::as_str).unwrap_or("").to_string();
                json!({
                    "id": o.id, "nom": e.conf.get("nom"), "suffixe": texte("suffixe", ""),
                    "description": texte("description", ""), "accent": texte("accent", "#888"),
                    "icone": texte("icone", ""), "depot": depot, "site": depot.strip_suffix(".git").unwrap_or(&depot),
                    "port": e.port, "dossier": dossier.to_string_lossy(), "installe": installe,
                    "phase": e.phase, "action": e.action, "message": e.message, "pid": e.pid,
                    "voulu": e.voulu, "maj_auto": e.maj_auto, "depuis": e.pid.map(|_| e.lance_le as i64),
                    "version": version, "maj": e.maj.json(), "code_appairage": code,
                    "terminal": terminal, "attend_saisie": e.attend_saisie, "sous_depots": sous,
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod essais {
    use super::*;

    #[test]
    fn decoupage_comme_shlex() {
        assert_eq!(decouper("--lan  --x=1").unwrap(), vec!["--lan", "--x=1"]);
        assert_eq!(decouper("'a b' \"c\\\"d\" e\\ f").unwrap(), vec!["a b", "c\"d", "e f"]);
        assert_eq!(decouper("; rm -rf ~").unwrap(), vec![";", "rm", "-rf", "~"]);
        assert!(decouper("'ouvert").is_err());
    }

    #[test]
    fn entiers_comme_python() {
        assert_eq!(entier(&json!("8080")), Some(8080));
        assert_eq!(entier(&json!(80.7)), Some(80));
        assert_eq!(entier(&json!("abc")), None);
        assert_eq!(entier(&json!(null)), None);
    }
}
