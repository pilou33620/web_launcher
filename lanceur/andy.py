"""Suivi d'Andy : les pesées ajoutées depuis la page, gardées dans andy.local.json (propre à ce Pi,
comme config.local.json). L'historique médical est dans andy_donnees.local.json, sur le Pi
seulement (jamais sur GitHub : le dépôt est public). Servi par /api/andy/donnees, après connexion.

Il était autrefois dans web/js/andy_donnees.json : s'il manque, il est repris de là, ou de
l'historique git (commit COMMIT_ANCIEN), une seule fois."""

import datetime
import json
import os
import subprocess
import threading
import uuid

from . import config

FICHIER = os.path.join(config.RACINE, "andy.local.json")
HISTORIQUE = os.path.join(config.RACINE, "andy_donnees.local.json")
ANCIEN = "web/js/andy_donnees.json"
COMMIT_ANCIEN = "6b778c7"          # dernier commit du dépôt qui contenait ANCIEN
NAISSANCE = datetime.date(2025, 11, 30)
POIDS_MIN, POIDS_MAX = 0.5, 100
MAX_PESEES = 5000
_verrou = threading.Lock()         # le serveur traite chaque requête dans son propre thread


def _lire():
    try:
        with open(FICHIER, encoding="utf-8") as f:
            brut = json.load(f).get("pesees", [])
    except (FileNotFoundError, ValueError, AttributeError):
        return []
    return [p for p in brut if isinstance(p, dict) and isinstance(p.get("id"), str)
            and isinstance(p.get("date"), str) and isinstance(p.get("poids"), (int, float))]


def _ecrire(pesees):
    temporaire = FICHIER + ".tmp"
    with open(temporaire, "w", encoding="utf-8") as f:
        json.dump({"pesees": pesees}, f, ensure_ascii=False, indent=1)
    os.replace(temporaire, FICHIER)


def lire():
    with _verrou:
        return sorted(_lire(), key=lambda p: p["date"])


def ajouter(date, poids, aujourdhui=None):
    """Ajoute une pesée et la renvoie. ValueError (message affichable) si la date ou le poids est invalide."""
    # un jour de marge sur « aujourd'hui » : l'appareil et le Pi ne sont pas forcément dans le même fuseau
    limite = (aujourdhui or datetime.date.today()) + datetime.timedelta(days=1)
    try:
        jour = datetime.date.fromisoformat(date)
    except (TypeError, ValueError):
        raise ValueError("Date invalide.")
    if not NAISSANCE <= jour <= limite:
        raise ValueError("La date doit être entre la naissance d'Andy et aujourd'hui.")
    if isinstance(poids, bool) or not isinstance(poids, (int, float)) or not POIDS_MIN <= poids <= POIDS_MAX:
        raise ValueError("Le poids doit être entre %s et %s kg." % (POIDS_MIN, POIDS_MAX))
    entree = {"id": uuid.uuid4().hex[:8], "date": jour.isoformat(), "poids": round(float(poids), 2)}
    with _verrou:
        pesees = _lire()
        if len(pesees) >= MAX_PESEES:
            raise ValueError("Trop de pesées enregistrées.")
        pesees.append(entree)
        _ecrire(pesees)
    return entree


def supprimer(identifiant):
    """True si la pesée existait."""
    with _verrou:
        pesees = _lire()
        restantes = [p for p in pesees if p["id"] != identifiant]
        if len(restantes) == len(pesees):
            return False
        _ecrire(restantes)
    return True


def _reprendre():
    """Contenu de l'ancien fichier suivi par git (encore présent, ou dans l'historique), ou None."""
    try:
        with open(os.path.join(config.RACINE, ANCIEN), "rb") as f:
            return f.read()
    except OSError:
        pass
    try:
        r = subprocess.run(["git", "-C", config.RACINE, "show", "%s:%s" % (COMMIT_ANCIEN, ANCIEN)],
                           capture_output=True, timeout=30)
    except (OSError, subprocess.SubprocessError):
        return None
    return r.stdout if r.returncode == 0 and r.stdout else None


def historique():
    """L'historique médical (liste), ou None s'il n'existe pas sur ce Pi."""
    with _verrou:
        if not os.path.exists(HISTORIQUE):
            brut = _reprendre()
            if brut is None:
                return None
            try:
                json.loads(brut.decode("utf-8"))
            except ValueError:
                return None
            fd = os.open(HISTORIQUE + ".tmp", os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
            with os.fdopen(fd, "wb") as f:
                f.write(brut)
            os.replace(HISTORIQUE + ".tmp", HISTORIQUE)
        try:
            with open(HISTORIQUE, encoding="utf-8") as f:
                return json.load(f)
        except (OSError, ValueError):
            return None
