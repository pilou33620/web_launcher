"""Sessions Claude Code remote-control (service utilisateur claude-remote) : quels dossiers de
Documents ont leur session, et leur redémarrage. La liste est lue par ~/.local/bin/claude-serveurs."""

import os
import re
import subprocess

from . import config

DOCUMENTS = os.path.dirname(config.RACINE)
LISTE = os.path.expanduser("~/.config/claude-serveurs.list")
SERVEURS = os.path.expanduser("~/.local/bin/claude-serveurs")
SERVICE = "claude-remote.service"
NOM_SUR = re.compile(r"^\w[\w.-]*$")    # passe tel quel dans la commande tmux du script
# systemctl --user et claude depuis le service système du launcher
ENV = dict(os.environ, XDG_RUNTIME_DIR="/run/user/%d" % os.getuid(),
           PATH=os.path.expanduser("~/.local/bin") + ":/usr/local/bin:/usr/bin:/bin")


def session(chemin):
    """Nom de la session tmux (et du serveur remote-control) d'un dossier, comme dans claude-serveurs."""
    return os.path.basename(chemin).replace(".", "_").replace(":", "_")


def _lancer(*cmd):
    r = subprocess.run(cmd, env=ENV, capture_output=True, text=True, timeout=60)
    if r.returncode:
        raise RuntimeError((r.stderr or r.stdout).strip() or "%s a échoué" % cmd[0])


def candidats():
    """Les dossiers de Documents, et leurs sous-dossiers qui sont des dépôts git."""
    trouves = []
    for nom in sorted(os.listdir(DOCUMENTS)):
        haut = os.path.join(DOCUMENTS, nom)
        if not NOM_SUR.match(nom) or nom.startswith(".") or not os.path.isdir(haut):
            continue
        trouves.append(nom)
        try:
            sous = sorted(os.listdir(haut))
        except OSError:
            continue
        trouves += [nom + "/" + s for s in sous if NOM_SUR.match(s) and not s.startswith(".")
                    and os.path.exists(os.path.join(haut, s, ".git"))]
    return trouves


def lire_liste():
    try:
        with open(LISTE, encoding="utf-8") as f:
            return [l.strip() for l in f if l.strip()]
    except FileNotFoundError:
        return []


def en_marche():
    r = subprocess.run(["tmux", "ls", "-F", "#{session_name}"], env=ENV, capture_output=True, text=True)
    return set(r.stdout.split()) if r.returncode == 0 else set()


def confiance_attendue(chemin):
    """Vrai si la session attend la réponse à « Trust <dossier> ? [y/N] » (1re ouverture d'un dossier)."""
    r = subprocess.run(["tmux", "capture-pane", "-p", "-t", "=" + session(chemin) + ":"],
                       env=ENV, capture_output=True, text=True)
    return r.returncode == 0 and "[y/N]" in r.stdout


def etat():
    actifs, tmux = lire_liste(), en_marche()
    return {"service": "claude" in tmux,
            "dossiers": [{"chemin": c, "actif": c in actifs, "en_marche": session(c) in tmux,
                          "confiance": c in actifs and session(c) in tmux and confiance_attendue(c)}
                         for c in candidats()]}


def _ouvrir():
    """Ouvre les sessions manquantes ; sans serveur tmux, démarre tout le service
    (sinon tmux naîtrait dans le service du launcher)."""
    if "claude" in en_marche():
        _lancer(SERVEURS)
    else:
        _lancer("systemctl", "--user", "start", SERVICE)


def activer(chemin, actif):
    if chemin not in candidats():
        raise ValueError("dossier inconnu")
    liste = [c for c in lire_liste() if c != chemin]
    if actif:
        nom = session(chemin)
        if nom == "claude" or any(session(c) == nom for c in liste):
            raise ValueError("une autre session s'appelle déjà « %s »" % nom)
        liste.append(chemin)
    os.makedirs(os.path.dirname(LISTE), exist_ok=True)
    with open(LISTE + ".tmp", "w", encoding="utf-8") as f:
        f.write("".join(c + "\n" for c in liste))
    os.replace(LISTE + ".tmp", LISTE)
    if actif:
        _ouvrir()
    elif session(chemin) in en_marche():
        _lancer("tmux", "kill-session", "-t", "=" + session(chemin))


def _tuer(chemin):
    if chemin not in lire_liste():
        raise ValueError("dossier sans session Claude")
    if session(chemin) in en_marche():
        _lancer("tmux", "kill-session", "-t", "=" + session(chemin))


def eteindre(chemin=None):
    """Sans chemin : tout le service. Le dossier reste dans la liste (revient au prochain démarrage)."""
    if chemin is None:
        return _lancer("systemctl", "--user", "stop", SERVICE)
    _tuer(chemin)


def allumer(chemin=None):
    """Sans chemin : tout le service. Avec : ouvre les sessions manquantes de la liste."""
    if chemin is None:
        return _lancer("systemctl", "--user", "start", SERVICE)
    if chemin not in lire_liste():
        raise ValueError("dossier sans session Claude")
    _ouvrir()


def faire_confiance(chemin):
    """Répond « y » à la question de confiance : le clic de l'utilisateur dans le launcher vaut accord."""
    if chemin not in lire_liste():
        raise ValueError("dossier sans session Claude")
    if not confiance_attendue(chemin):
        raise ValueError("aucune confirmation en attente")
    _lancer("tmux", "send-keys", "-t", "=" + session(chemin) + ":", "y", "Enter")


def redemarrer(chemin=None):
    """Sans chemin : tout le service (session Documents comprise)."""
    if chemin is None:
        return _lancer("systemctl", "--user", "restart", SERVICE)
    _tuer(chemin)
    _ouvrir()
