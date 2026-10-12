"""Opérations git d'un web tool : version, recherche et application des mises à jour, clonage.

Jamais de fusion ni d'écrasement : « git pull --ff-only » seulement, et rien si des
fichiers suivis ont été modifiés à la main. Git ne demande jamais de mot de passe
(un dépôt privé doit être accessible sans saisie : clé SSH ou jeton enregistré).
"""

import os
import shutil
import subprocess

DELAI = 120  # s


class ErreurGit(RuntimeError):
    """Message lisible, affiché tel quel dans l'interface."""


def _env():
    env = dict(os.environ, GIT_TERMINAL_PROMPT="0", LC_ALL="C")
    env.setdefault("GIT_SSH_COMMAND", "ssh -o BatchMode=yes")
    return env


def git(dossier, *args, delai=DELAI, verifier=True):
    try:
        r = subprocess.run(["git", *args], cwd=dossier, capture_output=True, text=True,
                           encoding="utf-8", errors="replace", timeout=delai, env=_env())
    except FileNotFoundError:
        raise ErreurGit("git n'est pas installé (sudo apt install git).")
    except subprocess.TimeoutExpired:
        raise ErreurGit("git ne répond pas (pas d'internet ?).")
    if verifier and r.returncode != 0:
        raise ErreurGit(expliquer(r.stderr or r.stdout))
    return r.stdout.strip()


def expliquer(sortie):
    """Dernière ligne utile de git, avec une piste pour les erreurs fréquentes."""
    texte = (sortie or "").strip()
    bas = texte.lower()
    if ("authentication" in bas or "could not read username" in bas
            or "permission denied (publickey)" in bas or "repository not found" in bas):
        return ("GitHub refuse l'accès (dépôt privé ?) : connecter le Raspberry Pi à GitHub, "
                "voir README « Dépôts privés ».")
    if "could not resolve host" in bas or "unable to access" in bas:
        return "GitHub injoignable (pas d'internet ?)."
    lignes = [l for l in texte.splitlines() if l.strip()]
    return lignes[-1] if lignes else "échec de git"


def est_depot(dossier):
    return os.path.isdir(os.path.join(dossier, ".git"))


def version(dossier):
    """{'commit', 'date', 'sujet'} du commit courant, ou None."""
    if not est_depot(dossier):
        return None
    try:
        sortie = git(dossier, "log", "-1", "--format=%h%x1f%cI%x1f%s", delai=10)
    except ErreurGit:
        return None
    commit, date, sujet = (sortie.split("\x1f") + ["", "", ""])[:3]
    return {"commit": commit, "date": date, "sujet": sujet}


def fichiers_modifies(dossier):
    """Fichiers suivis modifiés localement (ils bloqueraient la mise à jour)."""
    sortie = git(dossier, "status", "--porcelain", "--untracked-files=no", delai=20)
    return [l[3:] for l in sortie.splitlines() if l.strip()]


def verifier(dossier):
    """git fetch puis compare avec la branche suivie.

    Renvoie {'nombre', 'nouveautes', 'modifies'} ; lève ErreurGit."""
    if not est_depot(dossier):
        raise ErreurGit("pas un dépôt git (installé sans « git clone »).")
    git(dossier, "fetch", "--quiet")
    try:
        git(dossier, "rev-parse", "--abbrev-ref", "@{u}", delai=10)
    except ErreurGit:
        raise ErreurGit("aucune branche distante suivie.")
    nombre = int(git(dossier, "rev-list", "--count", "HEAD..@{u}", delai=20) or 0)
    nouveautes = git(dossier, "log", "--format=%s", "-10", "HEAD..@{u}", delai=20).splitlines() if nombre else []
    return {"nombre": nombre, "nouveautes": nouveautes, "modifies": fichiers_modifies(dossier)}


def appliquer(dossier):
    """Avance la copie locale. Renvoie la liste des fichiers changés (vide : déjà à jour)."""
    modifies = fichiers_modifies(dossier)
    if modifies:
        raise ErreurGit("fichiers modifiés sur le Raspberry Pi, mise à jour bloquée : "
                        + ", ".join(modifies[:5]) + (" …" if len(modifies) > 5 else ""))
    avant = git(dossier, "rev-parse", "HEAD", delai=10)
    git(dossier, "pull", "--ff-only", "--quiet")
    apres = git(dossier, "rev-parse", "HEAD", delai=10)
    if avant == apres:
        return []
    return git(dossier, "diff", "--name-only", avant, apres, delai=20).splitlines()


def cloner(url, cible):
    parent = os.path.dirname(cible)
    os.makedirs(parent, exist_ok=True)
    temporaire = cible + ".clonage"
    if os.path.exists(temporaire):
        shutil.rmtree(temporaire, ignore_errors=True)
    try:
        git(parent, "clone", "--quiet", url, temporaire, delai=900)
    except ErreurGit:
        shutil.rmtree(temporaire, ignore_errors=True)
        raise
    os.replace(temporaire, cible)
