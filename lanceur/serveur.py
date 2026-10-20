"""Serveur web du launcher : la page (web/) et l'API JSON utilisée par la page."""

import ipaddress
import json
import os
import re
import subprocess
import sys
import threading
import time
from http.cookies import CookieError, SimpleCookie
from http.server import SimpleHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import parse_qs, urlparse

from . import claude, compte, config, depot, reseau
from .gestionnaire import OccupeErreur

DOSSIER_WEB = os.path.join(config.RACINE, "web")
COOKIE = "launcher_session"
# Ouvert sans connexion : la page de connexion et ce qu'elle charge.
PUBLIC = {"/connexion.html", "/js/connexion.js"}


class Visites:
    """Les appareils qui utilisent ce serveur, affichés dans son terminal : une ligne quand
    une IP arrive, ou revient après INACTIF secondes sans requête (même format que les outils)."""
    INACTIF = 600

    def __init__(self, nom, afficher=None):
        self.nom = nom
        self.afficher = afficher or (lambda texte: print(texte, flush=True))
        self.verrou = threading.Lock()
        self.vues = {}                      # ip -> dernière requête (time.monotonic)

    def noter(self, ip, qui=None):
        try:
            a = ipaddress.ip_address(str(ip).split("%")[0])
            ip = str(getattr(a, "ipv4_mapped", None) or a)
        except ValueError:
            pass
        if qui:
            ip = "%s (%s)" % (ip, qui)
        maintenant = time.monotonic()
        with self.verrou:
            arrivee = maintenant - self.vues.get(ip, float("-inf")) > self.INACTIF
            self.vues = {i: t for i, t in self.vues.items() if maintenant - t <= self.INACTIF}
            self.vues[ip] = maintenant
            if not arrivee:
                return
            actifs = sorted(self.vues)
        self.dire(ip, "%d appareil(s) actif(s) : %s" % (len(actifs), ", ".join(actifs)))

    def dire(self, ip, texte):
        self.afficher("  [connexion] %s  %-11s %s  ->  %s" % (time.strftime("%H:%M:%S"), self.nom, ip, texte))


class Lanceur(SimpleHTTPRequestHandler):
    gestionnaire = None
    infos = {}                      # adresses, nom mDNS... (rempli par main)
    alias = None
    relancer = None                 # fonction : redémarre le launcher (après sa mise à jour)
    gardien = None                  # compte.Gardien : identifiant, mot de passe, sessions
    visites = Visites("launcher")   # les appareils connectés, dans la sortie du launcher
    timeout = 30                    # une connexion muette ne garde pas un thread pour toujours

    def __init__(self, *args, **kwargs):
        super().__init__(*args, directory=DOSSIER_WEB, **kwargs)

    def log_message(self, *args):   # pas une ligne par requête dans le journal
        pass

    # -- sécurité -------------------------------------------------------

    def _hote_permis(self):
        """Parade au DNS rebinding : le nom demandé doit désigner ce Raspberry Pi."""
        h = (self.headers.get("Host") or "").strip().lower().rstrip(".")
        if h.startswith("["):
            h = h[1:].split("]")[0]
        elif h.count(":") == 1:
            h = h.split(":")[0]
        nom = reseau.nom_machine()
        alias = (self.infos.get("nom_mdns") or "").lower()
        if h in ("", "localhost", nom, nom + ".local", alias, alias + ".local"):
            return True
        try:
            ipaddress.ip_address(h.split("%")[0])
            return True
        except ValueError:
            return False

    def parse_request(self):
        if not super().parse_request():
            return False
        if not self._hote_permis():
            self.send_error(403, "Nom d'hôte non autorisé")
            return False
        return True

    # -- connexion -------------------------------------------------------

    def _jeton_session(self):
        try:
            biscuit = SimpleCookie(self.headers.get("Cookie") or "")
        except CookieError:
            return None
        return biscuit[COOKIE].value if COOKIE in biscuit else None

    def ce_poste(self):
        try:
            ip = ipaddress.ip_address(self.client_address[0].split("%")[0])
        except (ValueError, IndexError, TypeError):
            return False
        return (getattr(ip, "ipv4_mapped", None) or ip).is_loopback

    def connecte(self):
        """Sans compte créé, seul ce poste entre (la page le signale en rouge) : un autre
        appareil du réseau ne peut pas prendre la main ni créer le premier compte."""
        self.login = self.gardien.utilisateur(self._jeton_session())
        if self.login is None and (self.gardien.actif or not self.ce_poste()):
            return False
        self.visites.noter(self.client_address[0], self.login)
        return True

    def _rediriger(self, cible):
        self.send_response(302)
        self.send_header("Location", cible)
        self.send_header("Content-Length", "0")
        self.end_headers()

    def end_headers(self):
        self.send_header("Cache-Control", "no-cache")
        self.send_header("X-Content-Type-Options", "nosniff")
        self.send_header("X-Frame-Options", "DENY")
        self.send_header("Referrer-Policy", "no-referrer")
        super().end_headers()

    # -- réponses -------------------------------------------------------

    def _json(self, donnees, statut=200, cookie=None):
        corps = json.dumps(donnees, ensure_ascii=False).encode("utf-8")
        self.send_response(statut)
        if cookie:
            self.send_header("Set-Cookie", cookie)
        self.send_header("Content-Type", "application/json; charset=utf-8")
        self.send_header("Content-Length", str(len(corps)))
        self.end_headers()
        self.wfile.write(corps)

    def _texte(self, texte, statut=200):
        corps = texte.encode("utf-8")
        self.send_response(statut)
        self.send_header("Content-Type", "text/plain; charset=utf-8")
        self.send_header("Content-Length", str(len(corps)))
        self.end_headers()
        self.wfile.write(corps)

    def etat(self):
        g = self.gestionnaire
        infos = dict(self.infos)
        infos["ip"] = reseau.ip_locale()
        port = infos["port"]
        suffixe = "" if port == 80 else ":%d" % port
        adresses = []
        if infos["ip"]:
            adresses.append({"type": "Réseau", "url": "http://%s%s/" % (infos["ip"], suffixe)})
        ts = reseau.ip_tailscale()
        if ts:
            adresses.append({"type": "Tailscale", "url": "http://%s%s/" % (ts, suffixe)})
        if infos.get("nom_mdns") and self.alias and self.alias.etat in ("annoncé", "nom de la machine"):
            adresses.append({"type": "Nom", "url": "http://%s.local%s/" % (infos["nom_mdns"], suffixe)})
        adresses.append({"type": "Machine", "url": "http://%s.local%s/" % (reseau.nom_machine(), suffixe)})
        infos.update(adresses=adresses, compte=self.gardien.actif, utilisateur=getattr(self, "login", None),
                     comptes=self.gardien.comptes(),
                     masques=[{"id": c["id"], "nom": c["nom"], "suffixe": c.get("suffixe", ""), "port": c["port"]}
                              for c in g.masques], mdns=self.alias.etat if self.alias else "désactivé",
                     version=depot.version(config.RACINE), maj=g.launcher_maj,
                     action=g.launcher_action, heure=int(time.time()),
                     heures_maj=g.heures_maj, prochaine_verif=g.prochaine_verif)
        return {"launcher": infos, "outils": g.etat()}

    # -- routes ---------------------------------------------------------

    def do_GET(self):
        url = urlparse(self.path)
        if url.path in PUBLIC or url.path.startswith(("/css/", "/icones/")):
            if url.path == "/connexion.html" and self.gardien.actif and self.connecte():
                return self._rediriger("/")
            return super().do_GET()
        if not self.connecte():
            if url.path.startswith("/api/"):
                return self._json({"erreur": "connexion requise"}, 401)
            return self._rediriger("/connexion.html")
        if url.path == "/api/etat":
            return self._json(self.etat())
        if url.path == "/api/claude":
            return self._json(claude.etat())
        if url.path.startswith("/api/journal/"):
            o = self.gestionnaire.par_id.get(url.path.rsplit("/", 1)[1])
            if o is None:
                return self._json({"erreur": "outil inconnu"}, 404)
            try:
                lignes = int(parse_qs(url.query).get("lignes", ["300"])[0])
            except ValueError:
                lignes = 300
            return self._texte(lire_fin(o.journal, lignes))
        if url.path.startswith("/api/console/"):
            o = self.gestionnaire.par_id.get(url.path.rsplit("/", 1)[1])
            if o is None:
                return self._json({"erreur": "outil inconnu"}, 404)
            try:
                depuis = int(parse_qs(url.query).get("depuis", ["-1"])[0])
            except ValueError:
                depuis = -1
            position, texte = lire_depuis(o.journal, depuis)
            return self._json({"position": position, "texte": texte, "pid": o.pid,
                               "phase": o.phase, "attend_saisie": o.attend_saisie,
                               "terminal": o.avec_terminal})
        if url.path.startswith("/api/"):
            return self._json({"erreur": "route inconnue"}, 404)
        if url.path not in ("/", "/index.html", "/console.html") and not url.path.startswith(("/css/", "/js/", "/icones/")):
            return self.send_error(404)
        return super().do_GET()

    def do_POST(self):
        # En-tête propre au launcher : un autre site ne peut pas l'envoyer sans
        # autorisation CORS (que le launcher ne donne jamais).
        if self.headers.get("X-Launcher") != "1":
            return self._json({"erreur": "requête refusée"}, 403)
        try:
            longueur = int(self.headers.get("Content-Length") or 0)
        except ValueError:
            return self._json({"erreur": "requête illisible"}, 400)
        if not 0 <= longueur <= 8192:   # négatif : read(-1) lirait sans fin
            return self._json({"erreur": "requête trop grande"}, 413)
        corps = self.rfile.read(longueur) if longueur else b""
        morceaux = urlparse(self.path).path.strip("/").split("/")
        if morceaux == ["api", "connexion"]:
            return self._connexion(corps)
        if morceaux == ["api", "deconnexion"]:
            self.gardien.fermer(self._jeton_session())
            return self._json({"ok": True}, cookie="%s=; Path=/; Max-Age=0; HttpOnly; SameSite=Strict" % COOKIE)
        if not self.connecte():
            return self._json({"erreur": "connexion requise"}, 401)
        g = self.gestionnaire
        try:
            if morceaux[:2] == ["api", "console"] and len(morceaux) == 3:
                try:
                    texte = json.loads(corps.decode("utf-8") or "{}").get("texte", "")
                except (ValueError, AttributeError):
                    return self._json({"erreur": "saisie illisible"}, 400)
                texte = filtrer_saisie(str(texte))
                if not texte:
                    return self._json({"erreur": "saisie vide"}, 400)
                try:
                    g.envoyer(morceaux[2], texte)
                except RuntimeError as exc:
                    return self._json({"erreur": str(exc)}, 409)
                return self._json({"ok": True})
            if morceaux[:2] == ["api", "claude"] and morceaux[2:] in (["activer"], ["redemarrer"], ["eteindre"], ["allumer"], ["confiance"]):
                return self._claude(morceaux[2], corps)
            if morceaux[:2] == ["api", "comptes"] and len(morceaux) == 3:
                return self._comptes(morceaux[2], corps)
            if morceaux[:2] == ["api", "outils"] and len(morceaux) == 4 and morceaux[3] == "reglages":
                try:
                    d = json.loads(corps.decode("utf-8") or "{}")
                    port, affiche = d.get("port"), d.get("affiche")
                except (ValueError, AttributeError):
                    return self._json({"erreur": "réglages illisibles"}, 400)
                try:
                    g.regler(morceaux[2], port=port, affiche=affiche)
                except ValueError as exc:
                    return self._json({"erreur": str(exc)}, 400)
                return self._json({"ok": True})
            if morceaux[:2] == ["api", "outils"] and len(morceaux) == 4 and morceaux[3] == "maj_auto":
                try:
                    valeur = json.loads(corps.decode("utf-8") or "{}").get("valeur")
                except (ValueError, AttributeError):
                    return self._json({"erreur": "demande illisible"}, 400)
                g.regler_maj_auto(morceaux[2], bool(valeur))
                return self._json({"ok": True})
            if morceaux[:2] == ["api", "outils"] and len(morceaux) == 4:
                g.lancer_action(morceaux[2], morceaux[3])
                self.visites.dire("%s (%s)" % (self.client_address[0], self.login or "sans compte"),
                                  "%s %s" % (morceaux[3], morceaux[2]))
                return self._json({"ok": True})
            if morceaux == ["api", "verifier_tout"]:
                for o in g.outils:
                    if o.installe:
                        try:
                            g.lancer_action(o.id, "verifier")
                        except OccupeErreur:
                            pass
                threading.Thread(target=g.verifier_launcher, daemon=True).start()
                return self._json({"ok": True})
            if morceaux == ["api", "launcher", "verifier"]:
                return self._json({"ok": True, "maj": g.verifier_launcher()})
            if morceaux == ["api", "launcher", "mettre_a_jour"]:
                return self._maj_launcher()
            if morceaux == ["api", "launcher", "redemarrer"]:
                if g.launcher_action:
                    return self._json({"erreur": "déjà en cours"}, 409)
                g.launcher_action = "Redémarrage"
                threading.Timer(0.8, self.relancer).start()
                return self._json({"ok": True, "message": "Redémarrage du launcher…"})
        except KeyError:
            return self._json({"erreur": "outil ou action inconnu"}, 404)
        except OccupeErreur as exc:
            return self._json({"erreur": str(exc)}, 409)
        return self._json({"erreur": "route inconnue"}, 404)

    def _connexion(self, corps):
        try:
            d = json.loads(corps.decode("utf-8") or "{}")
            login, mdp = str(d.get("login", ""))[:200], str(d.get("mot_de_passe", ""))[:1000]
        except (ValueError, AttributeError):
            return self._json({"erreur": "demande illisible"}, 400)
        jeton, erreur = self.gardien.verifier(login, mdp, self.client_address[0])
        # l'identifiant refusé n'est pas affiché : texte venu du réseau, pas dans le terminal
        self.visites.dire(self.client_address[0], "échec de connexion" if erreur else "connexion de %s" % login)
        if erreur:
            return self._json({"erreur": erreur}, 401)
        return self._json({"ok": True}, cookie="%s=%s; Path=/; Max-Age=%d; HttpOnly; SameSite=Strict"
                          % (COOKIE, jeton, compte.DUREE_SESSION))

    def _comptes(self, action, corps):
        """Ajouter un compte, changer un mot de passe, supprimer un compte (tous les comptes ont les mêmes droits)."""
        try:
            d = json.loads(corps.decode("utf-8") or "{}")
            login, mdp = str(d.get("login", ""))[:200], str(d.get("mot_de_passe", ""))[:1000]
        except (ValueError, AttributeError):
            return self._json({"erreur": "demande illisible"}, 400)
        if action == "ajouter":
            erreur = self.gardien.ajouter(login, mdp)
        elif action == "mot_de_passe":
            erreur = self.gardien.changer_mdp(login, mdp, garder=self._jeton_session())
        elif action == "supprimer":
            erreur = self.gardien.supprimer(login)
        else:
            return self._json({"erreur": "route inconnue"}, 404)
        return self._json({"erreur": erreur}, 400) if erreur else self._json({"ok": True})

    def _claude(self, action, corps):
        """Sessions Claude Code : ajouter/retirer un dossier, redémarrer/éteindre/allumer un dossier ou tout."""
        try:
            d = json.loads(corps.decode("utf-8") or "{}")
            chemin = d.get("chemin")
            if chemin is not None:
                chemin = str(chemin)[:300]
        except (ValueError, AttributeError):
            return self._json({"erreur": "demande illisible"}, 400)
        try:
            if action == "activer":
                claude.activer(chemin, bool(d.get("actif")))
            elif action == "confiance":
                claude.faire_confiance(chemin)
            else:
                {"redemarrer": claude.redemarrer, "eteindre": claude.eteindre,
                 "allumer": claude.allumer}[action](chemin)
        except ValueError as exc:
            return self._json({"erreur": str(exc)}, 400)
        except (RuntimeError, OSError, subprocess.SubprocessError) as exc:
            return self._json({"erreur": str(exc)[:300]}, 500)
        self.visites.dire("%s (%s)" % (self.client_address[0], self.login or "sans compte"),
                          "claude %s %s" % (action, chemin or "tout"))
        return self._json({"ok": True})

    def _maj_launcher(self):
        g = self.gestionnaire
        if g.launcher_action:
            return self._json({"erreur": "déjà en cours"}, 409)
        g.launcher_action = "Mise à jour"
        try:
            relancer = g.mettre_a_jour_launcher()
        except depot.ErreurGit as exc:
            g.launcher_action = None
            return self._json({"erreur": str(exc)}, 500)
        if not relancer:
            g.launcher_action = None
            return self._json({"ok": True, "message": "Déjà à jour."})
        g.launcher_action = "Redémarrage"
        threading.Timer(0.8, self.relancer).start()
        return self._json({"ok": True, "message": "Mis à jour, redémarrage du launcher…"})


JETON = re.compile(r"jeton=[\w-]+")
LOT_CONSOLE = 64 * 1024


def masquer(texte):
    # le jeton réseau de WEB·SUITE ouvre l'accès sans code d'appairage : jamais affiché
    return JETON.sub("jeton=•••", texte)


def filtrer_saisie(texte):
    """Texte, Entrée, Tab, effacement et Ctrl+C / Ctrl+D seulement : pas de Ctrl+Z (suspendre
    l'outil) ni de Ctrl+\\, ni de séquences d'échappement."""
    permis = {"\r", "\n", "\t", "\x03", "\x04", "\x7f"}
    return "".join(c for c in texte if c in permis or (c >= " " and c != "\x7f"))[:2000]


def lire_depuis(chemin, depuis):
    """(position, texte) : ce que le journal a reçu depuis la position (en octets).
    depuis < 0, ou journal renouvelé : les derniers 64 Ko."""
    try:
        with open(chemin, "rb") as f:
            f.seek(0, os.SEEK_END)
            taille = f.tell()
            if depuis < 0 or depuis > taille:
                depuis = max(0, taille - LOT_CONSOLE)
            f.seek(depuis)
            donnees = f.read(min(taille - depuis, LOT_CONSOLE))
    except OSError:
        return 0, ""
    # pas de caractère UTF-8 coupé en deux : la fin incomplète attend le lot suivant
    for recul in range(1, min(4, len(donnees)) + 1):
        octet = donnees[-recul]
        if octet & 0xC0 != 0x80:                    # premier octet d'un caractère
            besoin = 4 if octet >= 0xF0 else 3 if octet >= 0xE0 else 2 if octet >= 0xC0 else 1
            if besoin > recul:
                donnees = donnees[:-recul]
            break
    # un jeton pas encore écrit en entier attend aussi : il ne doit jamais sortir à moitié
    debut_ligne = donnees.rfind(b"\n") + 1
    if b"jeton=" in donnees[debut_ligne:]:
        donnees = donnees[:debut_ligne] if debut_ligne else donnees[:donnees.rfind(b"jeton=")]
    return depuis + len(donnees), masquer(donnees.decode("utf-8", "replace"))


def lire_fin(chemin, lignes):
    try:
        with open(chemin, "rb") as f:
            f.seek(0, os.SEEK_END)
            f.seek(max(0, f.tell() - 200 * 1024))
            texte = f.read().decode("utf-8", "replace")
    except OSError:
        return "(journal vide)"
    return masquer("\n".join(texte.splitlines()[-max(1, min(lignes, 3000)):]))


def ouvrir(port, port_secours, ecrire=print):
    """Ouvre le port voulu (80 pour « http://launcher.local » sans numéro), sinon le port de secours."""
    for p in (port, port_secours):
        if p is None:
            continue
        try:
            return ThreadingHTTPServer(("0.0.0.0", p), Lanceur)
        except PermissionError:
            ecrire("  Port %d refusé (il faut le service systemd du launcher, voir README) ; essai du port %s."
                   % (p, port_secours))
        except OSError as exc:
            ecrire("  Port %d indisponible (%s)." % (p, exc))
    raise SystemExit("Impossible d'ouvrir un port pour le launcher.")


def relancer_processus():
    """Remplace le processus par la nouvelle version (même PID : systemd et les outils ne voient rien)."""
    sys.stdout.flush()
    os.execv(sys.executable, [sys.executable, os.path.abspath(sys.argv[0])] + sys.argv[1:])
