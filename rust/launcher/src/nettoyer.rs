//! Supprime les branches déjà fusionnées dans origin/main, dépôt par dépôt.
//!
//! Parcourt tous les dépôts git sous la racine (par défaut ~/Documents). Une branche (locale ou sur
//! origin) n'est supprimée que si tout son contenu est déjà dans origin/main : elle en est ancêtre
//! (fusion classique), ou la fusionner dans main n'apporterait aucun changement (fusion squash ou
//! rebase). Les autres sont gardées. Demande confirmation pour chaque dépôt.
//!
//!     nettoyer_branches [racine]

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const PRINCIPALE: &str = "main";
const REF: &str = "origin/main";

fn git(depot: &Path, args: &[&str]) -> Output {
    Command::new("git").arg("-C").arg(depot).args(args).stdin(Stdio::null()).output().unwrap_or_else(|e| Output {
        status: std::os::unix::process::ExitStatusExt::from_raw(127 << 8),
        stdout: Vec::new(),
        stderr: e.to_string().into_bytes(),
    })
}

fn texte(o: &[u8]) -> String {
    String::from_utf8_lossy(o).trim().to_string()
}

pub fn depots(racine: &Path) -> Vec<PathBuf> {
    let mut trouves = Vec::new();
    let mut pile = vec![racine.to_path_buf()];
    while let Some(dossier) = pile.pop() {
        // .git en dossier seulement : un .git fichier est un worktree, déjà couvert par son dépôt
        if dossier.join(".git").is_dir() {
            trouves.push(dossier.clone());
        }
        let mut sous: Vec<PathBuf> = std::fs::read_dir(&dossier)
            .map(|d| {
                d.flatten()
                    .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
                    .filter(|e| e.file_name().to_str().is_some_and(|n| !n.starts_with('.') && n != "node_modules"))
                    .map(|e| e.path())
                    .collect()
            })
            .unwrap_or_default();
        sous.sort();
        pile.extend(sous.into_iter().rev());
    }
    trouves
}

/// Vrai si tout le contenu de `reference` est déjà dans origin/main.
fn fusionnee(depot: &Path, reference: &str) -> bool {
    if git(depot, &["merge-base", "--is-ancestor", reference, REF]).status.success() {
        return true;
    }
    let fusion = git(depot, &["merge-tree", "--write-tree", REF, reference]); // code 1 = conflit
    let arbre_main = texte(&git(depot, &["rev-parse", &format!("{}^{{tree}}", REF)]).stdout);
    fusion.status.success() && texte(&fusion.stdout).split_whitespace().next() == Some(arbre_main.as_str())
}

pub fn branches(depot: &Path, prefixe: &str) -> Vec<(String, String)> {
    let sortie = texte(&git(depot, &["for-each-ref", "--format=%(refname)\t%(objectname:short)", prefixe]).stdout);
    sortie
        .lines()
        .filter_map(|l| l.split_once('\t'))
        .filter_map(|(r, sha)| r.strip_prefix(prefixe).map(|n| (n.to_string(), sha.to_string())))
        .filter(|(n, _)| n != PRINCIPALE && n != "HEAD")
        .collect()
}

pub fn nettoyer(depot: &Path, confirmer: &mut dyn FnMut(&str) -> bool) {
    println!("\n== {}", depot.display());
    let fetch = git(depot, &["fetch", "--prune", "origin"]);
    if !fetch.status.success() {
        println!("  fetch impossible, dépôt ignoré : {}", texte(&fetch.stderr));
        return;
    }
    if !git(depot, &["rev-parse", "--verify", "-q", REF]).status.success() {
        println!("  pas de {}, dépôt ignoré", REF);
        return;
    }
    let courante = texte(&git(depot, &["branch", "--show-current"]).stdout);
    let locales: Vec<_> = branches(depot, "refs/heads/").into_iter().filter(|(n, _)| *n != courante).collect();
    let distantes = branches(depot, "refs/remotes/origin/");
    let mut a_suppr: Vec<(&str, String, String)> = Vec::new();
    for (n, s) in &locales {
        if fusionnee(depot, &format!("refs/heads/{}", n)) {
            a_suppr.push(("locale", n.clone(), s.clone()));
        }
    }
    for (n, s) in &distantes {
        if fusionnee(depot, &format!("refs/remotes/origin/{}", n)) {
            a_suppr.push(("origin", n.clone(), s.clone()));
        }
    }
    let gardees = locales.len() + distantes.len() - a_suppr.len();
    if a_suppr.is_empty() {
        println!("  rien à supprimer ({} branche(s) non fusionnée(s) gardée(s))", gardees);
        return;
    }
    for (ou, nom, sha) in &a_suppr {
        println!("  fusionnée  {:<6} {}  ({})", ou, nom, sha);
    }
    println!("  {} branche(s) non fusionnée(s) gardée(s)", gardees);
    if !confirmer(&format!("  Supprimer ces {} branche(s) ?", a_suppr.len())) {
        return;
    }
    for (ou, nom, sha) in &a_suppr {
        // -D : la fusion est déjà vérifiée ci-dessus, -d refuserait les squash
        let r = if *ou == "locale" { git(depot, &["branch", "-D", nom]) } else { git(depot, &["push", "origin", "--delete", nom]) };
        let etat = if r.status.success() {
            "supprimée".to_string()
        } else {
            format!("ÉCHEC : {}", texte(&r.stderr).lines().last().unwrap_or(""))
        };
        // le SHA permet de la recréer : git branch <nom> <sha>
        println!("  {}  {:<6} {}  ({})", etat, ou, nom, sha);
    }
}

fn confirmer(question: &str) -> bool {
    use std::io::Write;
    print!("{} [o/N] ", question);
    let _ = std::io::stdout().flush();
    let mut r = String::new();
    matches!(std::io::stdin().read_line(&mut r), Ok(n) if n > 0) && ["o", "oui"].contains(&r.trim().to_lowercase().as_str())
}

pub fn main() {
    let racine = std::env::args().nth(1).unwrap_or_else(|| "~/Documents".into());
    for depot in depots(&crate::config::chemin_absolu(&racine)) {
        nettoyer(&depot, &mut confirmer);
    }
}

#[cfg(test)]
mod essais {
    use super::*;

    fn g(depot: &Path, args: &[&str]) {
        let o = Command::new("git").arg("-C").arg(depot).args(["-c", "user.name=t", "-c", "user.email=t@t"]).args(args).output().unwrap();
        assert!(o.status.success(), "git {:?} : {}", args, String::from_utf8_lossy(&o.stderr));
    }

    fn commit(depot: &Path, fichier: &str, contenu: &str) {
        std::fs::write(depot.join(fichier), contenu).unwrap();
        g(depot, &["add", fichier]);
        g(depot, &["commit", "-m", &format!("{}{}", fichier, contenu)]);
    }

    #[test]
    fn supprime_seulement_le_fusionne() {
        let tmp = std::env::temp_dir().join(format!("lanceur-branches-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("racine")).unwrap();
        let (distant, d) = (tmp.join("distant.git"), tmp.join("racine/depot"));
        assert!(Command::new("git").args(["init", "-q", "--bare", "-b", "main"]).arg(&distant).status().unwrap().success());
        assert!(Command::new("git").args(["clone", "-q"]).arg(&distant).arg(&d).output().unwrap().status.success());
        g(&d, &["checkout", "-q", "-b", "main"]);
        commit(&d, "a", "1");
        for (nom, fichier) in [("classique", "b"), ("squash", "c"), ("encours", "e"), ("conflit", "a")] {
            g(&d, &["checkout", "-q", "-b", nom, "main"]);
            commit(&d, fichier, nom);
        }
        g(&d, &["checkout", "-q", "main"]);
        g(&d, &["merge", "-q", "--no-ff", "classique", "-m", "fusion"]);
        g(&d, &["merge", "-q", "--squash", "squash"]);
        g(&d, &["commit", "-m", "squash"]);
        commit(&d, "a", "2"); // main avance sur a : « conflit » n'est plus fusionnable proprement
        g(&d, &["push", "-q", "origin", "main", "classique", "squash", "encours", "conflit"]);

        for depot in depots(&tmp.join("racine")) {
            nettoyer(&depot, &mut |_| true);
        }
        let restent = |p: &str| {
            let mut n: Vec<String> = branches(&d, p).into_iter().map(|(n, _)| n).collect();
            n.sort();
            n
        };
        assert_eq!(restent("refs/heads/"), vec!["conflit", "encours"]);
        g(&d, &["fetch", "-q", "--prune", "origin"]);
        assert_eq!(restent("refs/remotes/origin/"), vec!["conflit", "encours"]);
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
