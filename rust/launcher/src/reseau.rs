//! Réseau : adresse du Raspberry Pi, ports, et le nom « launcher.local » (mDNS, via Avahi).

use std::net::{Ipv4Addr, SocketAddr, TcpStream, UdpSocket};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::{dans_le_path, tenir};

/// Adresse IPv4 du Raspberry Pi sur le réseau local (aucun paquet n'est envoyé).
pub fn ip_locale() -> Option<String> {
    let s = UdpSocket::bind("0.0.0.0:0").ok()?;
    s.connect("10.255.255.255:1").ok()?;
    Some(s.local_addr().ok()?.ip().to_string())
}

pub fn ip_tailscale() -> Option<String> {
    web_tools_commun::appairage::ip_tailscale().map(|ip| ip.to_string())
}

/// Le nom court de la machine, en minuscules.
pub fn nom_machine() -> String {
    web_tools_commun::appairage::nom_poste().split('.').next().unwrap_or("").to_lowercase()
}

pub fn port_ouvert(port: u16) -> bool {
    TcpStream::connect_timeout(&SocketAddr::from((Ipv4Addr::LOCALHOST, port)), Duration::from_millis(400)).is_ok()
}

/// Inodes des sockets TCP en écoute sur ce port (Linux, /proc/net/tcp*).
fn inodes_ecoute(port: u16) -> Vec<String> {
    let mut inodes = Vec::new();
    for fichier in ["/proc/net/tcp", "/proc/net/tcp6"] {
        let Ok(texte) = std::fs::read_to_string(fichier) else { continue };
        for ligne in texte.lines().skip(1) {
            let champs: Vec<&str> = ligne.split_whitespace().collect();
            if champs.len() > 9 && champs[3] == "0A" {
                let p = champs[1].rsplit(':').next().and_then(|h| u16::from_str_radix(h, 16).ok());
                if p == Some(port) {
                    inodes.push(champs[9].to_string());
                }
            }
        }
    }
    inodes
}

/// PID du programme qui écoute sur ce port, s'il appartient au même utilisateur (sinon None).
pub fn pid_sur_port(port: u16) -> Option<i32> {
    let cibles: Vec<String> = inodes_ecoute(port).iter().map(|i| format!("socket:[{}]", i)).collect();
    if cibles.is_empty() {
        return None;
    }
    for e in std::fs::read_dir("/proc").ok()?.flatten() {
        let nom = e.file_name().to_string_lossy().into_owned();
        let Ok(pid) = nom.parse::<i32>() else { continue };
        let Ok(fds) = std::fs::read_dir(e.path().join("fd")) else { continue };
        for fd in fds.flatten() {
            if let Ok(lien) = std::fs::read_link(fd.path()) {
                if cibles.iter().any(|c| lien.as_os_str() == c.as_str()) {
                    return Some(pid);
                }
            }
        }
    }
    None
}

/// Annonce « <nom>.local » sur le réseau local avec avahi-publish, et suit les changements
/// d'adresse IP (box redémarrée, Wi-Fi). Rien ne se passe si Avahi n'est pas installé, ou si la
/// machine porte déjà ce nom (Avahi l'annonce alors).
pub struct AliasMdns {
    nom: Option<String>,
    interne: Mutex<(Option<Child>, Option<String>, String)>, // processus, IP annoncée, état
}

impl AliasMdns {
    pub fn new(nom: Option<String>) -> Arc<AliasMdns> {
        Arc::new(AliasMdns { nom, interne: Mutex::new((None, None, "inactif".into())) })
    }

    pub fn etat(&self) -> String {
        tenir(&self.interne).2.clone()
    }

    fn regler(&self, etat: &str) {
        tenir(&self.interne).2 = etat.to_string();
    }

    pub fn demarrer(self: &Arc<Self>, journal: impl Fn(&str)) {
        let Some(nom) = self.nom.clone().filter(|n| !n.is_empty()) else {
            return self.regler("désactivé");
        };
        if nom_machine() == nom.to_lowercase() {
            return self.regler("nom de la machine");
        }
        if dans_le_path("avahi-publish").is_none() {
            self.regler("avahi-utils absent");
            journal(&format!("  mDNS : avahi-publish introuvable (sudo apt install avahi-utils), {}.local ne sera pas annoncé.", nom));
            return;
        }
        nettoyer_anciens(&nom);
        let moi = self.clone();
        std::thread::spawn(move || loop {
            if let Some(ip) = ip_locale() {
                let mut i = tenir(&moi.interne);
                let vivant = matches!(i.0.as_mut().map(|p| p.try_wait()), Some(Ok(None)));
                if i.1.as_deref() != Some(ip.as_str()) || !vivant {
                    arreter(&mut i.0);
                    let lance = Command::new("avahi-publish")
                        .args(["-a", "-R", &format!("{}.local", nom), &ip])
                        .stdin(Stdio::null())
                        .stdout(Stdio::null())
                        .stderr(Stdio::null())
                        .spawn();
                    match lance {
                        Ok(p) => *i = (Some(p), Some(ip), "annoncé".into()),
                        Err(e) => i.2 = format!("erreur : {}", e),
                    }
                }
            }
            std::thread::sleep(Duration::from_secs(30));
        });
    }

    pub fn arreter(&self) {
        arreter(&mut tenir(&self.interne).0);
    }
}

/// Le motif pkill des annonces de ce nom : « avahi-publish -a -R <nom>.local » en tête de ligne.
fn motif_annonce(nom: &str) -> String {
    let echappe: String = nom.chars().flat_map(|c| {
        let special = "\\.^$|?*+()[]{}".contains(c);
        special.then_some('\\').into_iter().chain(std::iter::once(c))
    }).collect();
    format!("^avahi-publish -a -R {}\\.local ", echappe)
}

/// Les annonces laissées par le démarrage précédent : systemd (KillMode=process, pour épargner
/// les outils) n'arrête que le launcher, son avahi-publish lui survit.
fn nettoyer_anciens(nom: &str) {
    let uid = nix::unistd::getuid().to_string();
    let _ = Command::new("pkill")
        .args(["-u", &uid, "-f", &motif_annonce(nom)])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// SIGTERM, puis SIGKILL après 5 s.
fn arreter(proc: &mut Option<Child>) {
    if let Some(mut p) = proc.take() {
        if matches!(p.try_wait(), Ok(None)) {
            let _ = nix::sys::signal::kill(nix::unistd::Pid::from_raw(p.id() as i32), nix::sys::signal::Signal::SIGTERM);
            let fin = Instant::now() + Duration::from_secs(5);
            while Instant::now() < fin && matches!(p.try_wait(), Ok(None)) {
                std::thread::sleep(Duration::from_millis(100));
            }
            let _ = p.kill();
            let _ = p.wait();
        }
    }
}

#[cfg(test)]
mod essais {
    #[test]
    fn motif_des_anciennes_annonces() {
        assert_eq!(motif_annonce("launcher"), "^avahi-publish -a -R launcher\\.local ");
        assert_eq!(motif_annonce("a.b"), "^avahi-publish -a -R a\\.b\\.local ");
    }

    use super::*;

    #[test]
    fn port_et_pid() {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        assert!(port_ouvert(port));
        assert_eq!(pid_sur_port(port), Some(std::process::id() as i32));
        drop(l);
        assert!(!port_ouvert(port));
    }
}
