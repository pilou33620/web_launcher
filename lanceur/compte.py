"""Comptes du launcher : un ou plusieurs (mêmes droits), mots de passe jamais stockés en clair.

- Mot de passe : haché avec scrypt (lent et gourmand en mémoire exprès, pour rendre
  inutile le vol du fichier), sel aléatoire de 16 octets. PBKDF2-SHA256 (600 000 tours)
  si scrypt manque. Fichier compte.local.json, lisible par son seul propriétaire.
- Sessions : jeton aléatoire de 256 bits dans un cookie HttpOnly, SameSite=Strict. Le
  serveur n'en garde que l'empreinte SHA-256 (sessions.local.json) : le fichier ne
  permet pas de se connecter. Valables 30 jours, gardées aux redémarrages du launcher.
- Essais : 5 erreurs depuis une même adresse bloquent cette adresse 5 minutes.

- Comptes : ajoutés, supprimés, mot de passe changé depuis la page (Réglages), ou en console.

    python3 launcher.py --compte     # ajouter un compte, ou changer un mot de passe
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


def lire_comptes(chemin=FICHIER):
    """Les comptes (liste). L'ancien fichier à un seul compte est lu comme une liste d'un."""
    try:
        with open(chemin, encoding="utf-8") as f:
            donnees = json.load(f)
    except (OSError, ValueError):
        return []
    comptes = donnees.get("comptes", [donnees]) if isinstance(donnees, dict) else []
    return [c for c in comptes if isinstance(c, dict) and c.get("login") and c.get("empreinte")]


def _trouver(comptes, login):
    login = str(login).strip().lower()
    return next((c for c in comptes if c["login"].lower() == login), None)


def verifier_login(login):
    """Message d'erreur, ou None si l'identifiant convient."""
    if not login:
        return "Identifiant vide."
    if len(login) > 40 or any(c in login for c in ' "\\') or not login.isprintable():
        return "Identifiant : 40 caractères au plus, sans espace ni guillemet."
    return None


def verifier_mdp(mdp):
    if len(mdp) < LONGUEUR_MIN:
        return "Mot de passe trop court (%d caractères au moins)." % LONGUEUR_MIN
    return None


class Gardien:
    """Vérifie les identifiants, tient les comptes et les sessions ouvertes.
    Tous les comptes ont les mêmes droits."""

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
        return bool(lire_comptes(self.fichier))

    # -- identifiants ----------------------------------------------------

    def verifier(self, login, mot_de_passe, ip):
        """Renvoie (jeton de session, None) ou (None, message d'erreur)."""
        with self.verrou:
            nombre, jusqu = self.echecs.get(ip, [0, 0])
            if time.time() < jusqu:
                return None, "Trop d'essais : réessayez dans %d min." % ((jusqu - time.time()) // 60 + 1)
        comptes = lire_comptes(self.fichier)
        if not comptes:
            return None, "Aucun compte : le créer sur le Raspberry Pi (python3 launcher.py --compte)."
        # identifiant inconnu : le calcul est fait quand même (même durée, rien n'est révélé)
        compte = _trouver(comptes, login)
        modele = compte or comptes[0]
        with self.calculs:
            essai = hacher(str(mot_de_passe), base64.b64decode(modele["sel"]), modele["algo"], modele["params"])
        bon = compte is not None and hmac.compare_digest(essai["empreinte"], compte["empreinte"])
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
            self._relire()
            self.sessions[self._empreinte(jeton)] = {"fin": time.time() + DUREE_SESSION, "login": compte["login"]}
            self._sauver_sessions()
        return jeton, None

    # -- comptes (depuis la page) ----------------------------------------

    def comptes(self):
        return [{"login": c["login"], "cree_le": c.get("cree_le")} for c in lire_comptes(self.fichier)]

    def ajouter(self, login, mot_de_passe):
        """Nouveau compte. Renvoie un message d'erreur, ou None."""
        login = str(login).strip()
        erreur = verifier_login(login) or verifier_mdp(mot_de_passe)
        if erreur:
            return erreur
        with self.calculs:
            nouveau = dict(hacher(mot_de_passe), login=login, cree_le=time.strftime("%Y-%m-%d %H:%M"))
        with self.verrou:
            comptes = lire_comptes(self.fichier)
            if _trouver(comptes, login):
                return "Le compte « %s » existe déjà." % login
            _ecrire(self.fichier, {"comptes": comptes + [nouveau]})
        return None

    def changer_mdp(self, login, mot_de_passe, garder=None):
        """Nouveau mot de passe ; les appareils connectés à ce compte sont déconnectés,
        sauf la session « garder » (celle qui fait le changement)."""
        erreur = verifier_mdp(mot_de_passe)
        if erreur:
            return erreur
        with self.calculs:
            empreinte = hacher(mot_de_passe)
        with self.verrou:
            comptes = lire_comptes(self.fichier)
            c = _trouver(comptes, login)
            if c is None:
                return "Compte inconnu."
            c.update(empreinte)
            _ecrire(self.fichier, {"comptes": comptes})
            self._fermer_compte(c["login"], garder)
        return None

    def supprimer(self, login):
        with self.verrou:
            comptes = lire_comptes(self.fichier)
            c = _trouver(comptes, login)
            if c is None:
                return "Compte inconnu."
            if len(comptes) == 1:
                return "C'est le dernier compte : il ne peut pas être supprimé."
            _ecrire(self.fichier, {"comptes": [x for x in comptes if x is not c]})
            self._fermer_compte(c["login"])
        return None

    def _fermer_compte(self, login, garder=None):
        self._relire()
        garder = self._empreinte(garder) if garder else None
        self.sessions = {k: v for k, v in self.sessions.items()
                         if k == garder or (v["login"] or "").lower() != login.lower()}
        self._sauver_sessions()

    # -- sessions --------------------------------------------------------

    @staticmethod
    def _empreinte(jeton):
        return hashlib.sha256(jeton.encode("utf-8")).hexdigest()

    def utilisateur(self, jeton):
        """Identifiant du compte de cette session, ou None (session absente, expirée, compte supprimé)."""
        if not jeton:
            return None
        with self.verrou:
            self._relire()
            s = self.sessions.get(self._empreinte(jeton))
        if not s or s["fin"] <= time.time():
            return None
        c = _trouver(lire_comptes(self.fichier), s["login"] or "")
        return c["login"] if c else None

    def session_valide(self, jeton):
        return self.utilisateur(jeton) is not None

    def fermer(self, jeton):
        with self.verrou:
            self._relire()
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

    def _relire(self):
        if self._date() != self._date_fichier:          # changé par « --compte » : relu
            self.sessions = self._lire_sessions()

    def _lire_sessions(self):
        self._date_fichier = self._date()
        try:
            with open(self.fichier_sessions, encoding="utf-8") as f:
                sessions = json.load(f)
        except (OSError, ValueError):
            return {}
        maintenant = time.time()
        comptes = lire_comptes(self.fichier)
        ancien = comptes[0]["login"] if len(comptes) == 1 else None
        propres = {}
        for k, v in sessions.items():
            if isinstance(v, (int, float)):                 # ancien format : le compte unique
                v = {"fin": v, "login": ancien}
            if isinstance(v, dict) and isinstance(v.get("fin"), (int, float)) and v["fin"] > maintenant:
                propres[k] = {"fin": v["fin"], "login": v.get("login")}
        return propres

    def _sauver_sessions(self):
        maintenant = time.time()
        self.sessions = {k: v for k, v in self.sessions.items() if v["fin"] > maintenant}
        try:
            _ecrire(self.fichier_sessions, self.sessions)
            self._date_fichier = self._date()
        except OSError:
            pass


def creer_compte(fichier=FICHIER, sessions=SESSIONS, saisir=input, secret=getpass.getpass, ecrire=print):
    """En console : ajoute un compte, ou change le mot de passe d'un compte existant
    (ses appareils connectés sont alors déconnectés). Les autres comptes ne bougent pas."""
    comptes = lire_comptes(fichier)
    ecrire("Comptes du Web Launcher" + (" (existants : %s)" % ", ".join(c["login"] for c in comptes)
                                        if comptes else " : aucun pour l'instant"))
    seul = comptes[0]["login"] if len(comptes) == 1 else None
    login = saisir("  Identifiant (nouveau ou existant)%s : " % (" [%s]" % seul if seul else "")).strip()
    login = login or seul or ""
    erreur = verifier_login(login)
    if erreur:
        ecrire("  %s Rien n'est changé." % erreur)
        return False
    existant = _trouver(comptes, login)
    ecrire("  Nouveau mot de passe de « %s »." % existant["login"] if existant
           else "  Nouveau compte « %s »." % login)
    while True:
        mdp = secret("  Mot de passe (%d caractères au moins) : " % LONGUEUR_MIN)
        if verifier_mdp(mdp):
            ecrire("  Trop court.")
            continue
        if secret("  Encore une fois : ") != mdp:
            ecrire("  Les deux saisies diffèrent.")
            continue
        break
    g = Gardien(fichier, sessions)
    erreur = g.changer_mdp(existant["login"], mdp) if existant else g.ajouter(login, mdp)
    if erreur:
        ecrire("  " + erreur)
        return False
    ecrire("  Compte enregistré (mot de passe haché)."
           + (" Les appareils connectés à ce compte sont déconnectés." if existant else ""))
    return True
