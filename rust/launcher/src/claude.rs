//! Sessions Claude Code remote-control (service utilisateur claude-remote) : quels dossiers de
//! Documents ont leur session, et leur redémarrage. La liste est lue par ~/.local/bin/claude-serveurs.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use serde_json::{json, Value};

use crate::{ecrire_atomique, sortie};

pub const SERVICE: &str = "claude-remote.service";

type Lancer = Box<dyn Fn(&[&str]) -> Result<(), String> + Send + Sync>;
type EnMarche = Box<dyn Fn() -> HashSet<String> + Send + Sync>;
type Confiance = Box<dyn Fn(&str) -> bool + Send + Sync>;

pub struct Claude {
    pub documents: PathBuf,
    pub liste: PathBuf,
    pub serveurs: String,
    /// Lance une commande (tmux, systemctl --user, claude-serveurs) ; remplacé par les bancs d'essai.
    pub lancer: Lancer,
    pub en_marche: EnMarche,
    pub confiance_attendue: Confiance,
}

fn maison() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default()
}

/// systemctl --user et claude depuis le service système du launcher.
fn commande(prog: &str) -> Command {
    let mut c = Command::new(prog);
    c.env("XDG_RUNTIME_DIR", format!("/run/user/{}", nix::unistd::getuid()))
        .env("PATH", format!("{}:/usr/local/bin:/usr/bin:/bin", maison().join(".local/bin").display()));
    c
}

fn executer(cmd: &[&str]) -> Result<std::process::Output, String> {
    let mut c = commande(cmd[0]);
    c.args(&cmd[1..]);
    match sortie(c, Duration::from_secs(60)) {
        Ok(r) => Ok(r),
        Err(crate::Echec::Lancement(e)) => Err(format!("{} : {}", cmd[0], e)),
        Err(crate::Echec::Delai) => Err(format!("{} ne répond pas", cmd[0])),
    }
}

/// Nom de la session tmux (et du serveur remote-control) d'un dossier, comme dans claude-serveurs.
pub fn session(chemin: &str) -> String {
    Path::new(chemin).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default().replace(['.', ':'], "_")
}

/// ^\w[\w.-]*$ : le nom passe tel quel dans la commande tmux du script.
fn nom_sur(nom: &str) -> bool {
    let mut c = nom.chars();
    c.next().is_some_and(|p| p.is_alphanumeric() || p == '_') && c.all(|x| x.is_alphanumeric() || "_.-".contains(x))
}

fn trie(dossier: &Path) -> Vec<String> {
    let mut noms: Vec<String> = std::fs::read_dir(dossier)
        .map(|d| d.flatten().filter_map(|e| e.file_name().to_str().map(str::to_string)).collect())
        .unwrap_or_default();
    noms.sort();
    noms
}

impl Claude {
    pub fn new(racine: &Path) -> Claude {
        Claude {
            documents: racine.parent().unwrap_or(Path::new("/")).to_path_buf(),
            liste: maison().join(".config/claude-serveurs.list"),
            serveurs: maison().join(".local/bin/claude-serveurs").to_string_lossy().into_owned(),
            lancer: Box::new(|cmd: &[&str]| {
                let r = executer(cmd)?;
                if r.status.success() {
                    return Ok(());
                }
                let texte = String::from_utf8_lossy(if r.stderr.is_empty() { &r.stdout } else { &r.stderr }).trim().to_string();
                Err(if texte.is_empty() { format!("{} a échoué", cmd[0]) } else { texte })
            }),
            en_marche: Box::new(|| match executer(&["tmux", "ls", "-F", "#{session_name}"]) {
                Ok(r) if r.status.success() => String::from_utf8_lossy(&r.stdout).split_whitespace().map(str::to_string).collect(),
                _ => HashSet::new(),
            }),
            confiance_attendue: Box::new(|chemin: &str| {
                // vrai si la session attend la réponse à « Trust <dossier> ? [y/N] » (1re ouverture d'un dossier)
                let cible = format!("={}:", session(chemin));
                matches!(executer(&["tmux", "capture-pane", "-p", "-t", &cible]),
                         Ok(r) if r.status.success() && String::from_utf8_lossy(&r.stdout).contains("[y/N]"))
            }),
        }
    }

    /// Les dossiers de Documents (sauf temp, simple dépôt de fichiers), et leurs sous-dossiers qui
    /// sont des dépôts git.
    pub fn candidats(&self) -> Vec<String> {
        let mut trouves = Vec::new();
        for nom in trie(&self.documents) {
            let haut = self.documents.join(&nom);
            if !nom_sur(&nom) || nom.starts_with('.') || nom == "temp" || !haut.is_dir() {
                continue;
            }
            trouves.push(nom.clone());
            for s in trie(&haut) {
                if nom_sur(&s) && !s.starts_with('.') && haut.join(&s).join(".git").exists() {
                    trouves.push(format!("{}/{}", nom, s));
                }
            }
        }
        trouves
    }

    pub fn lire_liste(&self) -> Vec<String> {
        std::fs::read_to_string(&self.liste)
            .map(|t| t.lines().map(str::trim).filter(|l| !l.is_empty()).map(str::to_string).collect())
            .unwrap_or_default()
    }

    pub fn etat(&self) -> Value {
        let (actifs, tmux) = (self.lire_liste(), (self.en_marche)());
        let dossiers: Vec<Value> = self
            .candidats()
            .into_iter()
            .map(|c| {
                let (actif, marche) = (actifs.contains(&c), tmux.contains(&session(&c)));
                json!({"chemin": c, "actif": actif, "en_marche": marche,
                       "confiance": actif && marche && (self.confiance_attendue)(&c)})
            })
            .collect();
        json!({"service": tmux.contains("claude"), "dossiers": dossiers})
    }

    /// Ouvre les sessions manquantes ; sans serveur tmux, démarre tout le service (sinon tmux
    /// naîtrait dans le service du launcher).
    fn ouvrir(&self) -> Result<(), String> {
        if (self.en_marche)().contains("claude") {
            (self.lancer)(&[&self.serveurs])
        } else {
            (self.lancer)(&["systemctl", "--user", "start", SERVICE])
        }
    }

    fn tuer_session(&self, chemin: &str) -> Result<(), String> {
        if (self.en_marche)().contains(&session(chemin)) {
            return (self.lancer)(&["tmux", "kill-session", "-t", &format!("={}", session(chemin))]);
        }
        Ok(())
    }

    pub fn activer(&self, chemin: Option<&str>, actif: bool) -> Result<(), Erreur> {
        let chemin = chemin.unwrap_or("");
        if !self.candidats().iter().any(|c| c == chemin) {
            return Err(Erreur::Valeur("dossier inconnu".into()));
        }
        let mut liste: Vec<String> = self.lire_liste().into_iter().filter(|c| c != chemin).collect();
        if actif {
            let nom = session(chemin);
            if nom == "claude" || liste.iter().any(|c| session(c) == nom) {
                return Err(Erreur::Valeur(format!("une autre session s'appelle déjà « {} »", nom)));
            }
            liste.push(chemin.to_string());
        }
        if let Some(d) = self.liste.parent() {
            std::fs::create_dir_all(d).map_err(|e| Erreur::Autre(e.to_string()))?;
        }
        let texte: String = liste.iter().map(|c| format!("{}\n", c)).collect();
        ecrire_atomique(&self.liste, texte.as_bytes(), false).map_err(|e| Erreur::Autre(e.to_string()))?;
        if actif {
            self.ouvrir().map_err(Erreur::Autre)
        } else {
            self.tuer_session(chemin).map_err(Erreur::Autre)
        }
    }

    fn dans_la_liste(&self, chemin: &str) -> Result<(), Erreur> {
        if self.lire_liste().iter().any(|c| c == chemin) {
            Ok(())
        } else {
            Err(Erreur::Valeur("dossier sans session Claude".into()))
        }
    }

    /// Sans chemin : tout le service. Le dossier reste dans la liste (revient au prochain démarrage).
    pub fn eteindre(&self, chemin: Option<&str>) -> Result<(), Erreur> {
        let Some(chemin) = chemin else {
            return (self.lancer)(&["systemctl", "--user", "stop", SERVICE]).map_err(Erreur::Autre);
        };
        self.dans_la_liste(chemin)?;
        self.tuer_session(chemin).map_err(Erreur::Autre)
    }

    /// Sans chemin : tout le service. Avec : ouvre les sessions manquantes de la liste.
    pub fn allumer(&self, chemin: Option<&str>) -> Result<(), Erreur> {
        let Some(chemin) = chemin else {
            return (self.lancer)(&["systemctl", "--user", "start", SERVICE]).map_err(Erreur::Autre);
        };
        self.dans_la_liste(chemin)?;
        self.ouvrir().map_err(Erreur::Autre)
    }

    /// Sans chemin : tout le service (session Documents comprise).
    pub fn redemarrer(&self, chemin: Option<&str>) -> Result<(), Erreur> {
        let Some(chemin) = chemin else {
            return (self.lancer)(&["systemctl", "--user", "restart", SERVICE]).map_err(Erreur::Autre);
        };
        self.dans_la_liste(chemin)?;
        self.tuer_session(chemin).map_err(Erreur::Autre)?;
        self.ouvrir().map_err(Erreur::Autre)
    }

    /// Répond « y » à la question de confiance : le clic de l'utilisateur dans le launcher vaut accord.
    pub fn faire_confiance(&self, chemin: Option<&str>) -> Result<(), Erreur> {
        let chemin = chemin.unwrap_or("");
        self.dans_la_liste(chemin)?;
        if !(self.confiance_attendue)(chemin) {
            return Err(Erreur::Valeur("aucune confirmation en attente".into()));
        }
        (self.lancer)(&["tmux", "send-keys", "-t", &format!("={}:", session(chemin)), "y", "Enter"]).map_err(Erreur::Autre)
    }
}

/// Valeur : 400 (demande refusée) ; Autre : 500 (commande en échec).
#[derive(Debug)]
pub enum Erreur {
    Valeur(String),
    Autre(String),
}

#[cfg(test)]
mod essais {
    use super::*;
    use std::sync::{Arc, Mutex};

    fn claude(nom: &str, lancees: &Arc<Mutex<Vec<Vec<String>>>>) -> (Claude, PathBuf) {
        let d = std::env::temp_dir().join(format!("lanceur-claude-{}-{}", nom, std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        for s in ["a/.git", "a/b/.git", "a/pas_git", "c", ".cache", "x y", "temp"] {
            std::fs::create_dir_all(d.join(s)).unwrap();
        }
        let l = lancees.clone();
        let mut c = Claude::new(&d.join("web_launcher"));
        c.documents = d.clone();
        c.liste = d.join("liste");
        c.serveurs = "claude-serveurs".into();
        c.lancer = Box::new(move |cmd: &[&str]| {
            l.lock().unwrap().push(cmd.iter().map(|s| s.to_string()).collect());
            Ok(())
        });
        c.en_marche = Box::new(|| HashSet::from(["claude".to_string()]));
        (c, d)
    }

    fn v(l: &[&str]) -> Vec<String> {
        l.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn dossiers_et_liste() {
        let lancees = Arc::new(Mutex::new(Vec::new()));
        let (c, d) = claude("dossiers", &lancees);
        assert_eq!(c.candidats(), v(&["a", "a/b", "c"]));
        for chemin in ["../etc", "x y", "a/pas_git", ".cache"] {
            assert!(matches!(c.activer(Some(chemin), true), Err(Erreur::Valeur(_))), "{chemin}");
        }
        c.activer(Some("a/b"), true).unwrap();
        c.activer(Some("c"), true).unwrap();
        c.activer(Some("a/b"), false).unwrap();
        assert_eq!(c.lire_liste(), v(&["c"]));
        assert!(matches!(c.redemarrer(Some("a/b")), Err(Erreur::Valeur(_)))); // plus dans la liste
        assert_eq!(*lancees.lock().unwrap(), vec![v(&["claude-serveurs"]), v(&["claude-serveurs"])]);
        lancees.lock().unwrap().clear();
        assert!(c.eteindre(Some("a/b")).is_err());
        assert!(c.allumer(Some("a/b")).is_err());
        c.eteindre(Some("c")).unwrap(); // reste dans la liste
        c.allumer(Some("c")).unwrap();
        c.eteindre(None).unwrap();
        c.allumer(None).unwrap();
        assert_eq!(c.lire_liste(), v(&["c"]));
        assert_eq!(
            *lancees.lock().unwrap(),
            vec![v(&["claude-serveurs"]), v(&["systemctl", "--user", "stop", SERVICE]), v(&["systemctl", "--user", "start", SERVICE])]
        );
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn confiance() {
        let lancees = Arc::new(Mutex::new(Vec::new()));
        let (mut c, d) = claude("confiance", &lancees);
        std::fs::write(&c.liste, "c\n").unwrap();
        assert!(c.faire_confiance(Some("autre")).is_err());
        c.confiance_attendue = Box::new(|_: &str| false);
        assert!(c.faire_confiance(Some("c")).is_err());
        c.confiance_attendue = Box::new(|_: &str| true);
        c.faire_confiance(Some("c")).unwrap();
        assert_eq!(*lancees.lock().unwrap(), vec![v(&["tmux", "send-keys", "-t", "=c:", "y", "Enter"])]);
        let _ = std::fs::remove_dir_all(d);
    }
}
