"""Compte du launcher : un identifiant et un mot de passe, jamais stocké en clair.

- Mot de passe : haché avec scrypt (lent et gourmand en mémoire exprès, pour rendre
  inutile le vol du fichier), sel aléatoire de 16 octets. PBKDF2-SHA256 (600 000 tours)
  si scrypt manque. Fichier compte.local.json, lisible par son seul propriétaire.
- Sessions : jeton aléatoire de 256 bits dans un cookie HttpOnly, SameSite=Strict. Le
  serveur n'en garde que l'empreinte SHA-256 (sessions.local.json) : le fichier ne
  permet pas de se connecter. Valables 30 jours, gardées aux redémarrages du launcher.
- Essais : 5 erreurs depuis une même adresse bloquent cette adresse 5 minutes.

    python3 launcher.py --compte     # créer ou changer l'identifiant et le mot de passe
"""

import base64
import getpass
import hashlib
import hmac
import json
import os
import secrets
import threading
import time

from . import config

FICHIER = os.path.join(config.RACINE, "compte.local.json")
SESSIONS = os.path.join(config.RACINE, "sessions.local.json")
DUREE_SESSION = 30 * 24 * 3600
ESSAIS, BLOCAGE = 5, 300
LONGUEUR_MIN = 8
SCRYPT = {"n": 2 ** 15, "r": 8, "p": 1}
PBKDF2_TOURS = 600000


def _b64(octets):
    return base64.b64encode(octets).decode("ascii")


def hacher(mot_de_passe, sel=None, algo=None, params=None):
    sel = sel or secrets.token_bytes(16)
    secret = mot_de_passe.encode("utf-8")
    if algo is None:
        algo = "scrypt" if hasattr(hashlib, "scrypt") else "pbkdf2_sha256"
    if algo == "scrypt":
        params = params or dict(SCRYPT)
        empreinte = hashlib.scrypt(secret, salt=sel, maxmem=128 * 1024 * 1024, dklen=32, **params)
    else:
        params = params or {"tours": PBKDF2_TOURS}
        empreinte = hashlib.pbkdf2_hmac("sha256", secret, sel, params["tours"])
    return {"algo": algo, "params": params, "sel": _b64(sel), "empreinte": _b64(empreinte)}


def _ecrire(chemin, donnees):
    temporaire = chemin + ".tmp"
    fd = os.open(temporaire, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    with os.fdopen(fd, "w", encoding="utf-8") as f:
        json.dump(donnees, f, indent=1)
    os.replace(temporaire, chemin)


def lire_compte(chemin=FICHIER):
    try:
        with open(chemin, encoding="utf-8") as f:
            compte = json.load(f)
        return compte if compte.get("login") and compte.get("empreinte") else None
    except (OSError, ValueError):
        return None


class Gardien:
    """Vérifie les identifiants et tient les sessions ouvertes."""

    def __init__(self, fichier=FICHIER, sessions=SESSIONS):
        self.fichier = fichier
        self.fichier_sessions = sessions
        self.verrou = threading.Lock()
        self.calculs = threading.Semaphore(2)   # scrypt prend 32 Mo : pas plus de 2 à la fois
        self.echecs = {}                    # ip -> [nombre, bloqué jusqu'à]
        self._date_fichier = None
        self.sessions = self._lire_sessions()

    @property
    def actif(self):
        return lire_compte(self.fichier) is not None

    # -- identifiants ----------------------------------------------------

    def verifier(self, login, mot_de_passe, ip):
        """Renvoie (jeton de session, None) ou (None, message d'erreur)."""
        with self.verrou:
            nombre, jusqu = self.echecs.get(ip, [0, 0])
            if time.time() < jusqu:
                return None, "Trop d'essais : réessayez dans %d min." % ((jusqu - time.time()) // 60 + 1)
        compte = lire_compte(self.fichier)
        if compte is None:
            return None, "Aucun compte : le créer sur le Raspberry Pi (python3 launcher.py --compte)."
        # le calcul est fait même si l'identifiant est faux : même durée, rien n'est révélé
        with self.calculs:
            essai = hacher(str(mot_de_passe), base64.b64decode(compte["sel"]), compte["algo"], compte["params"])
        bon = hmac.compare_digest(essai["empreinte"], compte["empreinte"])
        bon = hmac.compare_digest(str(login).strip().lower(), compte["login"].lower()) and bon
        with self.verrou:
            if not bon:
                nombre += 1
                self.echecs[ip] = [0, time.time() + BLOCAGE] if nombre >= ESSAIS else [nombre, 0]
                reste = ESSAIS - nombre
                return None, ("Identifiant ou mot de passe incorrect"
                              + (" (%d essai(s) restant(s))." % reste if 0 < reste < ESSAIS else
                                 " : adresse bloquée 5 min." if reste <= 0 else "."))
            self.echecs.pop(ip, None)
            jeton = secrets.token_urlsafe(32)
            self.sessions[self._empreinte(jeton)] = time.time() + DUREE_SESSION
            self._sauver_sessions()
        return jeton, None

    # -- sessions --------------------------------------------------------

    @staticmethod
    def _empreinte(jeton):
        return hashlib.sha256(jeton.encode("utf-8")).hexdigest()

    def session_valide(self, jeton):
        if not jeton:
            return False
        with self.verrou:
            if self._date() != self._date_fichier:      # changé par « --compte » : relu
                self.sessions = self._lire_sessions()
            fin = self.sessions.get(self._empreinte(jeton))
            return bool(fin and fin > time.time())

    def fermer(self, jeton):
        with self.verrou:
            if self.sessions.pop(self._empreinte(jeton or ""), None):
                self._sauver_sessions()

    def fermer_toutes(self):
        with self.verrou:
            self.sessions = {}
            self._sauver_sessions()

    def _date(self):
        try:
            return os.stat(self.fichier_sessions).st_mtime_ns
        except OSError:
            return None

    def _lire_sessions(self):
        self._date_fichier = self._date()
        try:
            with open(self.fichier_sessions, encoding="utf-8") as f:
                sessions = json.load(f)
        except (OSError, ValueError):
            return {}
        maintenant = time.time()
        return {k: v for k, v in sessions.items() if isinstance(v, (int, float)) and v > maintenant}

    def _sauver_sessions(self):
        maintenant = time.time()
        self.sessions = {k: v for k, v in self.sessions.items() if v > maintenant}
        try:
            _ecrire(self.fichier_sessions, self.sessions)
            self._date_fichier = self._date()
        except OSError:
            pass


def creer_compte(fichier=FICHIER, sessions=SESSIONS, saisir=input, secret=getpass.getpass, ecrire=print):
    """En console : identifiant et mot de passe (deux fois). Ferme toutes les sessions."""
    ancien = lire_compte(fichier)
    ecrire("Compte du Web Launcher" + (" (actuel : %s)" % ancien["login"] if ancien else ""))
    login = saisir("  Identifiant%s : " % (" [%s]" % ancien["login"] if ancien else "")).strip()
    if not login and ancien:
        login = ancien["login"]
    if not login:
        ecrire("  Identifiant vide : rien n'est changé.")
        return False
    while True:
        mdp = secret("  Mot de passe (%d caractères au moins) : " % LONGUEUR_MIN)
        if len(mdp) < LONGUEUR_MIN:
            ecrire("  Trop court.")
            continue
        if secret("  Encore une fois : ") != mdp:
            ecrire("  Les deux saisies diffèrent.")
            continue
        break
    compte = dict(hacher(mdp), login=login, cree_le=time.strftime("%Y-%m-%d %H:%M"))
    _ecrire(fichier, compte)
    try:
        os.remove(sessions)                 # les appareils connectés doivent se reconnecter
    except OSError:
        pass
    ecrire("  Compte enregistré (mot de passe haché, %s). Tous les appareils sont déconnectés."
           % compte["algo"])
    return True
